//! Container and volume sampling.
//!
//! - CPU % is differenced across our own ticks: `one_shot` stats leave
//!   `precpu_stats` zeroed, and a two-cycle read would wait ~1s per container.
//! - `started_at` is cached per container state, so steady state needs no
//!   inspect calls.

use super::host::unix_now;
use crate::icons::{ matcher::Subject, IconsHandle };
use bollard::{
    container::{ ListContainersOptions, MemoryStatsStats, Stats, StatsOptions },
    secret::{ ContainerSummary, MountPointTypeEnum, PortTypeEnum, VolumeScopeEnum },
    volume::ListVolumesOptions,
    Docker,
};
use helion_common::types::{
    ContainerHealth,
    ContainerInfo,
    ContainersResponse,
    PortInfo,
    PortType,
    VolumeInfo,
    VolumesResponse,
};
use futures_util::StreamExt;
use std::{ collections::{ HashMap, HashSet }, sync::Arc };

/// The serialized form for HTTP responses, plus the typed list for the
/// consumers inside the agent: the all-containers log stream follows the
/// running IDs, and the event log checks health and restarts. A few dozen
/// small structs, shared by `Arc`, not copied per reader.
pub struct ContainerSnapshot {
    pub json: Arc<str>,
    pub running: Arc<[String]>,
    pub containers: Arc<[ContainerInfo]>,
    pub sampled_at: i64,
}

impl ContainerSnapshot {
    /// The same list and sample time, marked as Docker not answering. Every
    /// consumer inside the agent sees exactly what it saw before, so nothing
    /// reads the outage as containers stopping or disappearing; only the
    /// app is told, so it can say so rather than show an empty list.
    pub fn unavailable(&self) -> Self {
        let response = ContainersResponse {
            containers: self.containers.to_vec(),
            sampled_at: self.sampled_at,
            docker_unavailable: true,
        };
        Self {
            json: encode(&response, "containers"),
            running: self.running.clone(),
            containers: self.containers.clone(),
            sampled_at: self.sampled_at,
        }
    }
}

pub struct VolumeSnapshot {
    pub json: Arc<str>,
}

fn encode<T: serde::Serialize>(value: &T, what: &str) -> Arc<str> {
    serde_json
        ::to_string(value)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to serialize {what}");
            "{}".to_string()
        })
        .into()
}

impl ContainerSnapshot {
    pub fn new(containers: Vec<ContainerInfo>) -> Self {
        let running = containers
            .iter()
            .filter(|c| c.state == "running")
            .map(|c| c.id.clone())
            .collect();
        let response = ContainersResponse { containers, sampled_at: unix_now(), docker_unavailable: false };
        Self {
            json: encode(&response, "containers"),
            running,
            sampled_at: response.sampled_at,
            containers: response.containers.into(),
        }
    }

    /// Before the first sample. `sampled_at` 0 tells the event log it isn't
    /// an empty host.
    pub fn empty() -> Self {
        Self { sampled_at: 0, ..Self::new(Vec::new()) }
    }
}

impl VolumeSnapshot {
    pub fn new(volumes: Vec<VolumeInfo>) -> Self {
        let response = VolumesResponse { volumes, sampled_at: unix_now() };
        Self { json: encode(&response, "volumes") }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

#[derive(Clone, Copy)]
struct CpuSample {
    total: u64,
    system: u64,
    cores: f64,
}

struct StartedEntry {
    state: String,
    started_at: Option<String>,
    restart_count: u32,
}

pub struct DockerProbe {
    prev_cpu: HashMap<String, CpuSample>,
    started: HashMap<String, StartedEntry>,
    concurrency: usize,
    icons: Option<IconsHandle>,
}

impl DockerProbe {
    pub fn new(concurrency: usize) -> Self {
        Self {
            prev_cpu: HashMap::new(),
            started: HashMap::new(),
            concurrency: concurrency.max(1),
            icons: None,
        }
    }

    pub fn with_icons(mut self, icons: Option<IconsHandle>) -> Self {
        self.icons = icons;
        self
    }

    pub async fn sample(
        &mut self,
        docker: &Docker
    ) -> Result<ContainerSnapshot, bollard::errors::Error> {
        let summaries = docker.list_containers(
            Some(ListContainersOptions::<String> { all: true, ..Default::default() })
        ).await?;

        // Only running containers have stats worth asking for; a stopped one
        // would cost a round trip to learn it uses no CPU.
        let running: Vec<String> = summaries
            .iter()
            .filter(|c| c.state.as_deref() == Some("running"))
            .filter_map(|c| c.id.clone())
            .collect();

        // Bounded so a busy host doesn't flood the Docker socket.
        let stats: HashMap<String, Stats> = futures_util::stream
            ::iter(running.into_iter().map(|id| {
                let docker = docker.clone();
                async move {
                    let mut stream = docker.stats(
                        &id,
                        Some(StatsOptions { stream: false, one_shot: true })
                    );
                    stream.next().await.and_then(|r| r.ok()).map(|s| (id, s))
                }
            }))
            .buffer_unordered(self.concurrency)
            .filter_map(|r| async move { r })
            .collect().await;

        let ids: HashSet<String> = summaries
            .iter()
            .filter_map(|c| c.id.clone())
            .collect();

        let mut containers = Vec::with_capacity(summaries.len());
        for summary in summaries {
            if let Some(info) = self.build(docker, summary, &stats).await {
                containers.push(info);
            }
        }

        // Drop bookkeeping for containers that no longer exist, so neither
        // map grows without bound on a host that churns containers.
        self.prev_cpu.retain(|id, _| ids.contains(id));
        self.started.retain(|id, _| ids.contains(id));

        containers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ContainerSnapshot::new(containers))
    }

    async fn build(
        &mut self,
        docker: &Docker,
        c: ContainerSummary,
        stats: &HashMap<String, Stats>
    ) -> Option<ContainerInfo> {
        let id = c.id?;
        let name = c.names
            .unwrap_or_default()
            .first()
            .cloned()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        let state = c.state.unwrap_or_default();
        let labels = c.labels.unwrap_or_default();

        let (cpu_pct, mem_used_bytes, mem_limit_bytes) = match stats.get(&id) {
            Some(s) => self.usage_from(&id, s),
            None => (0.0, 0, 0),
        };

        let (started_at, restart_count) = self.started_at(docker, &id, &state).await;

        let status = c.status.unwrap_or_default();
        let image = c.image.unwrap_or_default();
        let icon = self.icons.as_ref().and_then(|icons| {
            let (label, service) = (
                crate::labels::label(&labels, "icon"),
                crate::labels::label(&labels, "service"),
            );
            icons.resolve(
                &(Subject {
                    label: label.as_deref(),
                    service: service.as_deref(),
                    image: &image,
                    compose_service: labels.get("com.docker.compose.service").map(String::as_str),
                    name: &name,
                })
            )
        });
        Some(ContainerInfo {
            id,
            name,
            image,
            health: ContainerHealth::from_status(&status),
            status,
            state,
            ports: c.ports
                .unwrap_or_default()
                .into_iter()
                .map(|p| PortInfo {
                    ip: p.ip,
                    private_port: p.private_port,
                    public_port: p.public_port,
                    port_type: p.typ.and_then(|t| {
                        match t {
                            PortTypeEnum::TCP => Some(PortType::Tcp),
                            PortTypeEnum::UDP => Some(PortType::Udp),
                            PortTypeEnum::SCTP => Some(PortType::Sctp),
                            PortTypeEnum::EMPTY => None,
                        }
                    }),
                })
                .collect(),
            started_at,
            created_unix: c.created.unwrap_or(0),
            restart_count,
            compose_project: labels.get("com.docker.compose.project").cloned(),
            compose_service: labels.get("com.docker.compose.service").cloned(),
            update_labels: crate::labels::update_labels(&labels),
            helion_service: crate::labels::label(&labels, "service"),
            helion_service_description: crate::labels::label(&labels, "service.description"),
            helion_service_url: crate::labels::label(&labels, "service.url"),
            icon,
            cpu_pct,
            mem_used_bytes,
            mem_limit_bytes,
        })
    }

    fn usage_from(&mut self, id: &str, s: &Stats) -> (f32, u64, u64) {
        let current = CpuSample {
            total: s.cpu_stats.cpu_usage.total_usage,
            system: s.cpu_stats.system_cpu_usage.unwrap_or(0),
            cores: s.cpu_stats.online_cpus.unwrap_or(1).max(1) as f64,
        };

        let cpu_pct = match self.prev_cpu.get(id) {
            Some(prev) if current.system > prev.system && current.total >= prev.total => {
                let cpu_delta = (current.total - prev.total) as f64;
                let system_delta = (current.system - prev.system) as f64;
                ((cpu_delta / system_delta) * current.cores * 100.0) as f32
            }
            // First sighting, or the counters went backwards (container
            // restarted). Correct from the next tick.
            _ => 0.0,
        };
        self.prev_cpu.insert(id.to_string(), current);

        // Docker's raw `usage` includes page cache, which makes idle
        // containers look like they're holding hundreds of megabytes. The
        // `docker stats` CLI subtracts the inactive file cache; so do we.
        let usage = s.memory_stats.usage.unwrap_or(0);
        let cache = match &s.memory_stats.stats {
            Some(MemoryStatsStats::V1(v1)) => v1.total_inactive_file,
            Some(MemoryStatsStats::V2(v2)) => v2.inactive_file,
            None => 0,
        };

        (cpu_pct.max(0.0), usage.saturating_sub(cache), s.memory_stats.limit.unwrap_or(0))
    }

    /// `started_at` changes only when a container transitions state, so the
    /// inspect is issued on first sighting and on transitions only.
    async fn started_at(
        &mut self,
        docker: &Docker,
        id: &str,
        state: &str
    ) -> (Option<String>, u32) {
        if let Some(entry) = self.started.get(id) {
            if entry.state == state {
                return (entry.started_at.clone(), entry.restart_count);
            }
        }

        let inspected = docker.inspect_container(id, None).await.ok();
        let restart_count = inspected
            .as_ref()
            .and_then(|c| c.restart_count)
            .unwrap_or(0)
            .max(0) as u32;
        let started_at = inspected
            .and_then(|c| c.state)
            .and_then(|s| s.started_at);

        self.started.insert(id.to_string(), StartedEntry {
            state: state.to_string(),
            started_at: started_at.clone(),
            restart_count,
        });
        (started_at, restart_count)
    }
}

/// Volumes need only two API calls regardless of how many there are: bollard's
/// container summaries already carry their mount points, so the container ->
/// volume mapping is a local reshape rather than an inspect per volume.
pub async fn sample_volumes(docker: &Docker) -> Result<VolumeSnapshot, bollard::errors::Error> {
    let (volumes_result, containers_result) = tokio::join!(
        docker.list_volumes(Some(ListVolumesOptions::<String>::default())),
        docker.list_containers(
            Some(ListContainersOptions::<String> { all: true, ..Default::default() })
        )
    );

    let in_use_by = build_volume_usage_map(containers_result?);
    let volumes = volumes_result?.volumes.unwrap_or_default();

    let mut volumes: Vec<VolumeInfo> = volumes
        .into_iter()
        .map(|v| VolumeInfo {
            compose_project: v.labels.get("com.docker.compose.project").cloned(),
            helion_service: crate::labels::label(&v.labels, "service"),
            scope: v.scope.and_then(|s| {
                match s {
                    VolumeScopeEnum::LOCAL => Some("local".to_string()),
                    VolumeScopeEnum::GLOBAL => Some("global".to_string()),
                    VolumeScopeEnum::EMPTY => None,
                }
            }),
            in_use_by: in_use_by.get(&v.name).cloned().unwrap_or_default(),
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
            created_at: v.created_at,
        })
        .collect();

    volumes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(VolumeSnapshot::new(volumes))
}

fn build_volume_usage_map(containers: Vec<ContainerSummary>) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for c in containers {
        let name = c.names
            .unwrap_or_default()
            .first()
            .cloned()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();

        for mount in c.mounts.unwrap_or_default() {
            // Skip bind mounts; only named volumes belong here.
            if mount.typ != Some(MountPointTypeEnum::VOLUME) {
                continue;
            }
            if let Some(volume_name) = mount.name {
                let entry = map.entry(volume_name).or_default();
                if !entry.contains(&name) {
                    entry.push(name.clone());
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::secret::MountPoint;

    #[test]
    fn docker_going_away_keeps_the_list_and_says_so() {
        let before = ContainerSnapshot {
            sampled_at: 1_000,
            ..ContainerSnapshot::new(vec![ContainerInfo {
                id: "abc".into(),
                name: "gitea".into(),
                image: "gitea:1".into(),
                status: "Up".into(),
                state: "running".into(),
                health: None,
                ports: vec![],
                started_at: None,
                created_unix: 0,
                restart_count: 0,
                compose_project: None,
                compose_service: None,
                update_labels: Default::default(),
                helion_service: None,
                helion_service_description: None,
                helion_service_url: None,
                icon: None,
                cpu_pct: 0.0,
                mem_used_bytes: 0,
                mem_limit_bytes: 0,
            }])
        };
        let down = before.unavailable();
        assert_eq!(down.sampled_at, 1_000, "the detectors see the same sample");
        assert_eq!(down.containers.len(), 1);
        assert_eq!(&*down.running, ["abc".to_string()].as_slice());
        let json: serde_json::Value = serde_json::from_str(&down.json).unwrap();
        assert_eq!(json["docker_unavailable"], true);
        assert_eq!(json["containers"][0]["name"], "gitea");

        let never = ContainerSnapshot::empty().unavailable();
        assert_eq!(never.sampled_at, 0, "never sampled stays never sampled");
    }

    fn summary(name: &str, volumes: &[&str]) -> ContainerSummary {
        ContainerSummary {
            names: Some(vec![format!("/{name}")]),
            mounts: Some(
                volumes
                    .iter()
                    .map(|v| MountPoint {
                        typ: Some(MountPointTypeEnum::VOLUME),
                        name: Some((*v).to_string()),
                        ..Default::default()
                    })
                    .collect()
            ),
            ..Default::default()
        }
    }

    #[test]
    fn volume_usage_map_groups_containers_per_volume() {
        let map = build_volume_usage_map(
            vec![summary("gitea", &["gitea-data"]), summary("nextcloud", &["gitea-data", "nc-data"])]
        );

        assert_eq!(map["gitea-data"], vec!["gitea", "nextcloud"]);
        assert_eq!(map["nc-data"], vec!["nextcloud"]);
    }

    #[test]
    fn bind_mounts_are_not_counted_as_volumes() {
        let c = ContainerSummary {
            names: Some(vec!["/caddy".to_string()]),
            mounts: Some(
                vec![MountPoint {
                    typ: Some(MountPointTypeEnum::BIND),
                    name: Some("/etc/caddy".to_string()),
                    ..Default::default()
                }]
            ),
            ..Default::default()
        };
        assert!(build_volume_usage_map(vec![c]).is_empty());
    }

    #[test]
    fn a_container_mounting_the_same_volume_twice_is_listed_once() {
        let map = build_volume_usage_map(vec![summary("immich", &["data", "data"])]);
        assert_eq!(map["data"], vec!["immich"]);
    }

    fn stats_with(total: u64, system: u64, cores: u64) -> Stats {
        let json =
            serde_json::json!({
            "read": "2024-01-01T00:00:00Z",
            "preread": "2024-01-01T00:00:00Z",
            "num_procs": 0,
            "pids_stats": {},
            "storage_stats": {},
            "blkio_stats": {},
            "cpu_stats": {
                "cpu_usage": {
                    "total_usage": total,
                    "usage_in_usermode": 0,
                    "usage_in_kernelmode": 0
                },
                "system_cpu_usage": system,
                "online_cpus": cores,
                "throttling_data": {
                    "periods": 0, "throttled_periods": 0, "throttled_time": 0
                }
            },
            // Zeroed exactly as Docker returns it under `one_shot: true` —
            // which is the whole reason we difference across our own ticks.
            "precpu_stats": {
                "cpu_usage": {
                    "total_usage": 0,
                    "usage_in_usermode": 0,
                    "usage_in_kernelmode": 0
                },
                "throttling_data": {
                    "periods": 0, "throttled_periods": 0, "throttled_time": 0
                }
            },
            "memory_stats": { "usage": 500_000_000u64, "limit": 2_000_000_000u64 },
            "name": "/test",
            "id": "abc"
        });
        serde_json::from_value(json).expect("stats fixture must deserialize")
    }

    #[test]
    fn cpu_is_zero_on_first_sighting_then_differenced_across_ticks() {
        let mut probe = DockerProbe::new(4);

        // precpu_stats is zeroed under one_shot, so the first tick has nothing
        // to difference against. Reporting 0 beats reporting nonsense.
        let (first, _, _) = probe.usage_from("abc", &stats_with(1_000, 10_000, 4));
        assert_eq!(first, 0.0);

        // 500 of 10_000 system ticks across 4 cores = 20%.
        let (second, _, _) = probe.usage_from("abc", &stats_with(1_500, 20_000, 4));
        assert!((second - 20.0).abs() < 0.001, "expected 20%, got {second}");
    }

    #[test]
    fn cpu_resets_rather_than_spiking_when_counters_go_backwards() {
        let mut probe = DockerProbe::new(4);
        probe.usage_from("abc", &stats_with(9_000, 20_000, 2));
        // Container restarted: total_usage drops. Must not underflow into a
        // huge percentage.
        let (pct, _, _) = probe.usage_from("abc", &stats_with(10, 30_000, 2));
        assert_eq!(pct, 0.0);
    }

    #[test]
    fn memory_excludes_page_cache() {
        let mut probe = DockerProbe::new(1);
        let mut s = stats_with(1, 2, 1);
        s.memory_stats.stats = Some(
            MemoryStatsStats::V2(serde_json::from_value(
                serde_json::json!({
                    "anon": 0u64, "file": 0u64, "kernel_stack": 0u64, "slab": 0u64,
                    "sock": 0u64, "shmem": 0u64, "file_mapped": 0u64, "file_dirty": 0u64,
                    "file_writeback": 0u64, "anon_thp": 0u64, "inactive_anon": 0u64,
                    "active_anon": 0u64, "inactive_file": 300_000_000u64,
                    "active_file": 0u64, "unevictable": 0u64, "slab_reclaimable": 0u64,
                    "slab_unreclaimable": 0u64, "pgfault": 0u64, "pgmajfault": 0u64,
                    "workingset_refault": 0u64, "workingset_activate": 0u64,
                    "workingset_nodereclaim": 0u64, "pgrefill": 0u64, "pgscan": 0u64,
                    "pgsteal": 0u64, "pgactivate": 0u64, "pgdeactivate": 0u64,
                    "pglazyfree": 0u64, "pglazyfreed": 0u64, "thp_fault_alloc": 0u64,
                    "thp_collapse_alloc": 0u64
                })
            ).expect("v2 fixture"))
        );

        let (_, mem, limit) = probe.usage_from("abc", &s);
        assert_eq!(mem, 200_000_000, "500MB usage minus 300MB inactive file cache");
        assert_eq!(limit, 2_000_000_000);
    }
}
