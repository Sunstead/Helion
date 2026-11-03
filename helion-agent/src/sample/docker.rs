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
use std::{ collections::{ HashMap, HashSet }, sync::Arc, time::Instant };

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

/// A container's absolute counters at one of our ticks. Rates are differenced
/// against the previous one, never against Docker's `precpu_stats`, which
/// `one_shot` zeroes.
#[derive(Clone, Copy)]
struct Counters {
    at: Instant,
    cpu_total: u64,
    cpu_system: u64,
    cores: f64,
    /// `None` when the container has no networks of its own.
    net: Option<(u64, u64)>,
    /// `None` when the cgroup reports no block IO counters.
    blk: Option<(u64, u64)>,
}

/// What one tick's stats give a container.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Usage {
    cpu_pct: f32,
    mem_used: u64,
    mem_limit: u64,
    net: Option<(f64, f64)>,
    blk: Option<(f64, f64)>,
}

/// Sums received and sent bytes over the container's networks.
fn net_counters(s: &Stats) -> Option<(u64, u64)> {
    let nets = s.networks.as_ref().filter(|n| !n.is_empty())?;
    Some(nets.values().fold((0, 0), |(rx, tx), n| (rx + n.rx_bytes, tx + n.tx_bytes)))
}

/// Bytes read and written. cgroup v1 spells the ops `Read`/`Write`, v2
/// `read`/`write`; v1 also lists a `Total` we must not add in.
fn blk_counters(s: &Stats) -> Option<(u64, u64)> {
    let entries = s.blkio_stats.io_service_bytes_recursive.as_ref().filter(|e| !e.is_empty())?;
    Some(
        entries.iter().fold((0, 0), |(r, w), e| {
            if e.op.eq_ignore_ascii_case("read") {
                (r + e.value, w)
            } else if e.op.eq_ignore_ascii_case("write") {
                (r, w + e.value)
            } else {
                (r, w)
            }
        })
    )
}

/// A per-second rate from two readings of a counter. `None` when the counter
/// went backwards (the container restarted) or no time passed.
fn rate(now: u64, prev: u64, secs: f64) -> Option<f64> {
    (now >= prev && secs > 0.0).then(|| ((now - prev) as f64) / secs)
}

struct StartedEntry {
    state: String,
    started_at: Option<String>,
    restart_count: u32,
}

pub struct DockerProbe {
    prev: HashMap<String, Counters>,
    started: HashMap<String, StartedEntry>,
    concurrency: usize,
    icons: Option<IconsHandle>,
}

impl DockerProbe {
    pub fn new(concurrency: usize) -> Self {
        Self {
            prev: HashMap::new(),
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
        self.prev.retain(|id, _| ids.contains(id));
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

        let usage = match stats.get(&id) {
            Some(s) => self.usage_from(&id, s, Instant::now()),
            None => Usage::default(),
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
            cpu_pct: usage.cpu_pct,
            mem_used_bytes: usage.mem_used,
            mem_limit_bytes: usage.mem_limit,
            net_rx_bps: usage.net.map(|(rx, _)| rx),
            net_tx_bps: usage.net.map(|(_, tx)| tx),
            blk_read_bps: usage.blk.map(|(r, _)| r),
            blk_write_bps: usage.blk.map(|(_, w)| w),
        })
    }

    fn usage_from(&mut self, id: &str, s: &Stats, at: Instant) -> Usage {
        let current = Counters {
            at,
            cpu_total: s.cpu_stats.cpu_usage.total_usage,
            cpu_system: s.cpu_stats.system_cpu_usage.unwrap_or(0),
            cores: s.cpu_stats.online_cpus.unwrap_or(1).max(1) as f64,
            net: net_counters(s),
            blk: blk_counters(s),
        };

        let prev = self.prev.insert(id.to_string(), current);
        let cpu_pct = match prev {
            Some(prev) if
                current.cpu_system > prev.cpu_system &&
                current.cpu_total >= prev.cpu_total
            => {
                let cpu_delta = (current.cpu_total - prev.cpu_total) as f64;
                let system_delta = (current.cpu_system - prev.cpu_system) as f64;
                ((cpu_delta / system_delta) * current.cores * 100.0) as f32
            }
            // First sighting, or the counters went backwards (container
            // restarted). Correct from the next tick.
            _ => 0.0,
        };

        // A container with networks reports 0 B/s on first sighting rather
        // than nothing, so the field doesn't flicker between null and a value.
        let secs = prev.map_or(0.0, |p| at.duration_since(p.at).as_secs_f64());
        let pair = |now: Option<(u64, u64)>, before: Option<(u64, u64)>| {
            let (a, b) = now?;
            Some(match before {
                Some((pa, pb)) => (rate(a, pa, secs).unwrap_or(0.0), rate(b, pb, secs).unwrap_or(0.0)),
                None => (0.0, 0.0),
            })
        };
        let net = pair(current.net, prev.and_then(|p| p.net));
        let blk = pair(current.blk, prev.and_then(|p| p.blk));

        // Docker's raw `usage` includes page cache, which makes idle
        // containers look like they're holding hundreds of megabytes. The
        // `docker stats` CLI subtracts the inactive file cache; so do we.
        let usage = s.memory_stats.usage.unwrap_or(0);
        let cache = match &s.memory_stats.stats {
            Some(MemoryStatsStats::V1(v1)) => v1.total_inactive_file,
            Some(MemoryStatsStats::V2(v2)) => v2.inactive_file,
            None => 0,
        };

        Usage {
            cpu_pct: cpu_pct.max(0.0),
            mem_used: usage.saturating_sub(cache),
            mem_limit: s.memory_stats.limit.unwrap_or(0),
            net,
            blk,
        }
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
                net_rx_bps: None,
                net_tx_bps: None,
                blk_read_bps: None,
                blk_write_bps: None,
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
        let t = Instant::now();
        let first = probe.usage_from("abc", &stats_with(1_000, 10_000, 4), t).cpu_pct;
        assert_eq!(first, 0.0);

        // 500 of 10_000 system ticks across 4 cores = 20%.
        let second = probe.usage_from("abc", &stats_with(1_500, 20_000, 4), t + SECS_2).cpu_pct;
        assert!((second - 20.0).abs() < 0.001, "expected 20%, got {second}");
    }

    #[test]
    fn cpu_resets_rather_than_spiking_when_counters_go_backwards() {
        let mut probe = DockerProbe::new(4);
        let t = Instant::now();
        probe.usage_from("abc", &stats_with(9_000, 20_000, 2), t);
        // Container restarted: total_usage drops. Must not underflow into a
        // huge percentage.
        let pct = probe.usage_from("abc", &stats_with(10, 30_000, 2), t + SECS_2).cpu_pct;
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

        let u = probe.usage_from("abc", &s, Instant::now());
        assert_eq!(u.mem_used, 200_000_000, "500MB usage minus 300MB inactive file cache");
        assert_eq!(u.mem_limit, 2_000_000_000);
    }

    const SECS_2: std::time::Duration = std::time::Duration::from_secs(2);

    /// Adds network and block IO counters to the fixture.
    fn with_io(mut s: Stats, rx: u64, tx: u64, read: u64, write: u64) -> Stats {
        s.networks = Some(
            serde_json::from_value(
                serde_json::json!({
                "eth0": { "rx_bytes": rx - 100, "tx_bytes": tx, "rx_dropped": 0, "rx_errors": 0,
                          "rx_packets": 0, "tx_dropped": 0, "tx_errors": 0, "tx_packets": 0 },
                "eth1": { "rx_bytes": 100, "tx_bytes": 0, "rx_dropped": 0, "rx_errors": 0,
                          "rx_packets": 0, "tx_dropped": 0, "tx_errors": 0, "tx_packets": 0 }
            })
            ).unwrap()
        );
        s.blkio_stats = serde_json::from_value(
            serde_json::json!({
            "io_service_bytes_recursive": [
                { "major": 8, "minor": 0, "op": "Read", "value": read },
                { "major": 8, "minor": 0, "op": "Write", "value": write },
                { "major": 8, "minor": 0, "op": "Total", "value": read + write }
            ]
        })
        ).unwrap();
        s
    }

    #[test]
    fn network_and_block_io_are_rates_summed_over_interfaces() {
        let mut probe = DockerProbe::new(1);
        let t = Instant::now();
        let first = probe.usage_from("abc", &with_io(stats_with(1, 2, 1), 1_000, 0, 0, 0), t);
        assert_eq!(first.net, Some((0.0, 0.0)), "a value from the start, not null");

        let next = with_io(stats_with(1, 2, 1), 5_000, 2_000, 4_096, 8_192);
        let u = probe.usage_from("abc", &next, t + SECS_2);
        assert_eq!(u.net, Some((2_000.0, 1_000.0)), "both interfaces, per second");
        // `Total` is not added to read and write.
        assert_eq!(u.blk, Some((2_048.0, 4_096.0)));
    }

    #[test]
    fn a_container_without_networks_reports_none() {
        let mut probe = DockerProbe::new(1);
        let u = probe.usage_from("abc", &stats_with(1, 2, 1), Instant::now());
        assert_eq!((u.net, u.blk), (None, None));
    }

    #[test]
    fn io_counters_going_backwards_read_as_zero() {
        let mut probe = DockerProbe::new(1);
        let t = Instant::now();
        probe.usage_from("abc", &with_io(stats_with(1, 2, 1), 9_000, 9_000, 9_000, 9_000), t);
        let u = probe.usage_from("abc", &with_io(stats_with(1, 2, 1), 200, 0, 0, 0), t + SECS_2);
        assert_eq!(u.net, Some((0.0, 0.0)));
        assert_eq!(u.blk, Some((0.0, 0.0)));
    }
}
