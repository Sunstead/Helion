//! Sampler orchestration. Each source is sampled on one schedule and published
//! to a `watch` channel, so cost and rate deltas are independent of client
//! count. `watch` over `broadcast`: subscribers only want the latest value.

// The parsers are pure, so their tests run everywhere; only Linux reads procfs.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod diskstats;
pub mod docker;
pub mod facts;
pub mod filters;
pub mod host;
pub mod tailnet;
pub mod wol;

use crate::{
    backups::{ BackupSnapshot, BackupsProvider },
    config::Config,
    docker::DockerHandle,
    history::HistoryHandle,
    icons::IconsHandle,
};
use docker::{ ContainerSnapshot, DockerProbe, VolumeSnapshot };
use facts::HostFacts;
use filters::HostFilters;
use futures_util::StreamExt;
use host::{ HostProbe, HostSnapshot };
use std::{ sync::Arc, time::{ Duration, Instant } };
use tokio::sync::watch;

/// Host metrics, sampled on a dedicated OS thread.
///
/// A thread rather than a tokio task because `sysinfo` does blocking `/proc`
/// reads, and because the loop owns `&mut System` for its entire life — so
/// there is no lock anywhere on this path, and a slow `/proc` can never stall
/// the HTTP runtime.
pub fn spawn_host(
    cfg: &Config,
    facts: Arc<HostFacts>,
    history: Option<HistoryHandle>
) -> watch::Receiver<Arc<HostSnapshot>> {
    let filters = Arc::new(HostFilters::from_config(&cfg.host));
    let period = Duration::from_millis(cfg.host.interval_ms);

    let mut probe = HostProbe::new(filters);
    let first = Arc::new(HostSnapshot::new(0, probe.sample(), &facts));
    let (tx, rx) = watch::channel(first);

    std::thread::Builder
        ::new()
        .name("host-sampler".into())
        .spawn(move || {
            let mut next = Instant::now();
            let mut seq: u64 = 0;

            loop {
                // Absolute-deadline scheduling: no cumulative drift. If we
                // fell behind (host suspend, heavy load), snap forward rather
                // than spin through a burst of catch-up ticks, which is what
                // tokio's default MissedTickBehavior::Burst would do.
                next += period;
                let now = Instant::now();
                if next < now {
                    next = now + period;
                }
                std::thread::sleep(next.saturating_duration_since(now));

                seq = seq.wrapping_add(1);
                let sample = probe.sample();

                if let Some(h) = &history {
                    h.push(&sample);
                }

                // Errors here mean every receiver was dropped, which only
                // happens during shutdown — AppState holds one for the life
                // of the process.
                if tx.send(Arc::new(HostSnapshot::new(seq, sample, &facts))).is_err() {
                    tracing::debug!("host sampler stopping: no receivers");
                    return;
                }
            }
        })
        .expect("spawn host-sampler");

    rx
}

/// Containers. Async because it's all Docker I/O.
pub fn spawn_containers(
    cfg: &Config,
    docker: DockerHandle,
    icons: Option<IconsHandle>
) -> watch::Receiver<Arc<ContainerSnapshot>> {
    let (tx, rx) = watch::channel(Arc::new(ContainerSnapshot::empty()));
    let period = Duration::from_millis(cfg.docker.interval_ms);
    let mut probe = DockerProbe::new(cfg.docker.stats_concurrency).with_icons(icons);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut failing = false;

        loop {
            // Either the interval elapses, or a Docker event says something
            // changed. The event path is what makes the action buttons feel
            // instant instead of waiting out a 2s tick.
            tokio::select! {
                _ = ticker.tick() => {}
                _ = docker.containers_changed().notified() => {}
            }

            let Some(client) = docker.try_reconnect() else {
                if !failing {
                    tracing::warn!("docker is unreachable; containers are the last known list");
                    failing = true;
                    let marked = tx.borrow().unavailable();
                    if tx.send(Arc::new(marked)).is_err() {
                        return;
                    }
                }
                continue;
            };

            match probe.sample(&client).await {
                Ok(snapshot) => {
                    if failing {
                        tracing::info!("docker container sampling recovered");
                        failing = false;
                    }
                    if tx.send(Arc::new(snapshot)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    // Log the transition, not every tick — otherwise a dead
                    // socket writes a line every two seconds forever.
                    if !failing {
                        tracing::warn!(error = %e, "docker container sampling failed");
                        failing = true;
                        let marked = tx.borrow().unavailable();
                        if tx.send(Arc::new(marked)).is_err() {
                            return;
                        }
                    }
                    docker.mark_disconnected();
                }
            }
        }
    });

    rx
}

/// Volumes change rarely, so the timer is slow; Docker events and actions
/// wake it for anything that happens in between. A sample is two list calls,
/// cheap enough to run on every change.
pub fn spawn_volumes(docker: DockerHandle) -> watch::Receiver<Arc<VolumeSnapshot>> {
    let (tx, rx) = watch::channel(Arc::new(VolumeSnapshot::empty()));

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut failing = false;

        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = docker.volumes_changed().notified() => {}
            }

            let Some(client) = docker.client() else {
                continue;
            };
            match docker::sample_volumes(&client).await {
                Ok(snapshot) => {
                    failing = false;
                    if tx.send(Arc::new(snapshot)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    if !failing {
                        tracing::warn!(error = %e, "docker volume sampling failed");
                        failing = true;
                    }
                }
            }
        }
    });

    rx
}

/// A Docker event for the container tracker.
pub struct Forwarded {
    pub event: bollard::models::EventMessage,
    /// From before this subscription: replayed, already handled by whoever
    /// was listening then (or nobody, if the agent was restarting).
    pub replayed: bool,
}

/// Watches the Docker event stream. With `poke`, wakes the samplers an event
/// affects; with `forward`, hands every event to the event log's container
/// tracker. A full forward queue drops events rather than stall the pokes.
///
/// Each subscription starts [`KILL_WINDOW`](crate::events::containers::KILL_WINDOW)
/// in the past. A deploy recreates this agent along with everything else, so
/// the `kill` for a container that takes a few seconds to stop can land while
/// no agent is listening; replayed, it still explains the `die` that follows.
pub fn spawn_event_watcher(
    docker: DockerHandle,
    poke: bool,
    forward: Option<tokio::sync::mpsc::Sender<Forwarded>>
) {
    tokio::spawn(async move {
        loop {
            let Some(client) = docker.client() else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };

            let mut filters = std::collections::HashMap::new();
            filters.insert("type".to_string(), vec![
                "container".to_string(),
                "volume".to_string()
            ]);

            let subscribed = host::unix_now();
            let since = subscribed - crate::events::containers::KILL_WINDOW;
            let mut events = client.events(
                Some(bollard::system::EventsOptions {
                    since: Some(since.to_string()),
                    until: None,
                    filters,
                })
            );

            tracing::debug!("watching docker events");
            while let Some(event) = events.next().await {
                match event {
                    Ok(event) => {
                        let replayed = event.time.is_some_and(|t| t < subscribed);
                        if poke && !replayed {
                            poke_for_event(&docker, &event);
                        }
                        if let Some(tx) = &forward {
                            let _ = tx.try_send(Forwarded { event, replayed });
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "docker event stream ended");
                        break;
                    }
                }
            }

            // The stream ends when dockerd restarts. Back off before
            // resubscribing so a down daemon isn't hammered.
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// Volume events change the volume list. Container create and destroy change
/// which containers use a volume; the rest (start, health, exec, ...) don't,
/// and health checks alone would otherwise resample volumes every few seconds.
fn poke_for_event(docker: &DockerHandle, event: &bollard::models::EventMessage) {
    use bollard::models::EventMessageTypeEnum as Kind;
    match event.typ {
        Some(Kind::VOLUME) => docker.poke_volumes(),
        Some(Kind::CONTAINER) => {
            docker.poke_containers();
            if matches!(event.action.as_deref(), Some("create" | "destroy")) {
                docker.poke_volumes();
            }
        }
        _ => {}
    }
}

/// Backups. A file poll, so it's cheap enough to run on the async runtime.
/// Polls every 30 s, or every 2 s while a requested backup or restore test
/// is queued or running, so the page follows it. `poke` rereads at once, as
/// after writing a request.
pub fn spawn_backups(cfg: &Config) -> (watch::Receiver<Arc<BackupSnapshot>>, Arc<tokio::sync::Notify>) {
    let mut provider = BackupsProvider::new(cfg.backups.clone());
    let initial = provider.poll(false).expect("first backups poll always yields a snapshot");
    let (tx, rx) = watch::channel(Arc::new(initial));
    let poke = Arc::new(tokio::sync::Notify::new());
    let woken = poke.clone();

    tokio::spawn(async move {
        loop {
            let busy = crate::backups::in_flight(&tx.borrow().status);
            let wait = Duration::from_secs(if busy { 2 } else { 30 });
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = woken.notified() => {}
            }
            if let Some(snapshot) = provider.poll(true) {
                if tx.send(Arc::new(snapshot)).is_err() {
                    return;
                }
            }
        }
    });

    (rx, poke)
}

/// Identifies the agent's own container so actions against it can be refused.
pub fn spawn_self_identification(docker: DockerHandle, hostname: String) {
    tokio::spawn(async move {
        let Some(client) = docker.client() else {
            return;
        };
        if let Some(id) = crate::docker::resolve_self_id(&client, &hostname).await {
            tracing::info!(container = %&id[..12.min(id.len())], "identified own container");
            docker.set_self_id(Some(id));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ EventMessage, EventMessageTypeEnum as Kind };
    use std::time::Duration;

    fn event(typ: Kind, action: &str) -> EventMessage {
        EventMessage { typ: Some(typ), action: Some(action.into()), ..Default::default() }
    }

    /// Whether a poke is waiting, without consuming more than a moment.
    async fn poked(n: &tokio::sync::Notify) -> bool {
        tokio::time::timeout(Duration::from_millis(20), n.notified()).await.is_ok()
    }

    fn handle() -> DockerHandle {
        DockerHandle::new("/nonexistent/docker.sock".into())
    }

    #[tokio::test]
    async fn volume_events_wake_only_the_volume_sampler() {
        let docker = handle();
        poke_for_event(&docker, &event(Kind::VOLUME, "destroy"));
        assert!(poked(docker.volumes_changed()).await);
        assert!(!poked(docker.containers_changed()).await);
    }

    #[tokio::test]
    async fn container_create_and_destroy_also_refresh_volumes() {
        for action in ["create", "destroy"] {
            let docker = handle();
            poke_for_event(&docker, &event(Kind::CONTAINER, action));
            assert!(poked(docker.containers_changed()).await, "{action}");
            assert!(poked(docker.volumes_changed()).await, "{action}");
        }
    }

    #[tokio::test]
    async fn noisy_container_events_leave_volumes_alone() {
        let docker = handle();
        poke_for_event(&docker, &event(Kind::CONTAINER, "health_status: healthy"));
        assert!(poked(docker.containers_changed()).await);
        assert!(!poked(docker.volumes_changed()).await);
    }

    /// The bug this replaced: `notify_waiters` dropped a poke that arrived
    /// while the sampler was busy sampling, so it waited out a full tick.
    #[tokio::test]
    async fn a_poke_while_nobody_waits_is_kept() {
        let docker = handle();
        docker.poke_volumes();
        assert!(poked(docker.volumes_changed()).await);
        // One permit, not a backlog.
        assert!(!poked(docker.volumes_changed()).await);
    }
}
