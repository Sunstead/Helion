//! The one Docker client, plus the mutating operations.
//!
//! Built once and shared, so the connection pool is reused. If Docker is
//! unreachable at startup the agent still runs (host metrics don't need it)
//! and the sampler retries the connection.

use crate::error::{ from_docker, AgentError };
use bollard::{
    container::{
        ListContainersOptions,
        LogsOptions,
        RemoveContainerOptions,
        RestartContainerOptions,
        StartContainerOptions,
        StopContainerOptions,
    },
    Docker,
};
use helion_common::types::{ ContainerAction, ContainerActionResult };
use std::{ sync::{ Arc, RwLock }, time::{ Duration, Instant } };

/// How long Docker calls must keep failing, with the socket still there,
/// before the agent restarts itself to get a fresh bind mount.
pub const STALE_SOCKET_AFTER: Duration = Duration::from_secs(60);
/// And in at least this many attempts, so one slow failure can't decide it.
pub const STALE_SOCKET_MIN_FAILURES: u32 = 5;

#[derive(Clone)]
pub struct DockerHandle {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` while Docker is unreachable. Read-mostly: written only by the
    /// sampler when a reconnect succeeds or the socket goes away.
    client: RwLock<Option<Docker>>,
    socket: String,
    /// The agent's own container, when it's running in one. Actions against
    /// it are refused so a stray click can't cut off access to the node.
    self_id: RwLock<Option<String>>,
    /// Poked by the event watcher and by actions so each sampler runs now
    /// instead of waiting out its interval. One per sampler, and poked with
    /// `notify_one`: that stores a permit when the sampler is mid-sample, so
    /// a change that lands during a sample triggers another one rather than
    /// being lost until the next tick.
    containers_changed: tokio::sync::Notify,
    volumes_changed: tokio::sync::Notify,
    /// Fired once by the container sampler when the socket looks stale (see
    /// [`FailureStreak`]); `main` then shuts down and exits non-zero.
    restart_needed: tokio::sync::Notify,
}

impl DockerHandle {
    pub fn new(socket: String) -> Self {
        let client = connect(&socket);
        if client.is_none() {
            tracing::warn!(
                socket = %socket,
                "docker unreachable at startup; host metrics will still be served"
            );
        }
        Self {
            inner: Arc::new(Inner {
                client: RwLock::new(client),
                socket,
                self_id: RwLock::new(None),
                containers_changed: tokio::sync::Notify::new(),
                volumes_changed: tokio::sync::Notify::new(),
                restart_needed: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn client(&self) -> Option<Docker> {
        self.inner.client.read().ok()?.clone()
    }

    /// Used by handlers, which must fail loudly rather than return an empty
    /// list when Docker is down.
    pub fn require(&self) -> Result<Docker, AgentError> {
        self.client().ok_or_else(||
            AgentError::DockerUnavailable(format!("cannot reach docker at {}", self.inner.socket))
        )
    }

    pub fn containers_changed(&self) -> &tokio::sync::Notify {
        &self.inner.containers_changed
    }

    pub fn volumes_changed(&self) -> &tokio::sync::Notify {
        &self.inner.volumes_changed
    }

    /// Something about the container list changed.
    pub fn poke_containers(&self) {
        self.inner.containers_changed.notify_one();
    }

    /// Something about volumes changed, including which containers use them.
    pub fn poke_volumes(&self) {
        self.inner.volumes_changed.notify_one();
    }

    /// Called by the sampler when the client is absent. Cheap — it verifies
    /// the socket exists and builds a client; no round trip.
    pub fn try_reconnect(&self) -> Option<Docker> {
        if let Some(existing) = self.client() {
            return Some(existing);
        }
        let fresh = connect(&self.inner.socket);
        if fresh.is_some() {
            tracing::info!(socket = %self.inner.socket, "docker connection established");
            if let Ok(mut slot) = self.inner.client.write() {
                *slot = fresh.clone();
            }
        }
        fresh
    }

    /// Whether something is at the socket path. Inside a container whose
    /// compose file bind-mounts the socket file itself, this stays true after
    /// dockerd restarts: the mount still holds the old, deleted inode.
    pub fn socket_exists(&self) -> bool {
        std::path::Path::new(&self.inner.socket).exists()
    }

    pub fn socket(&self) -> &str {
        &self.inner.socket
    }

    /// Asks `main` to stop the agent so its supervisor starts it again.
    pub fn request_restart(&self) {
        self.inner.restart_needed.notify_one();
    }

    /// Resolves once [`Self::request_restart`] has been called.
    pub async fn restart_requested(&self) {
        self.inner.restart_needed.notified().await;
    }

    pub fn mark_disconnected(&self) {
        if let Ok(mut slot) = self.inner.client.write() {
            if slot.is_some() {
                tracing::warn!(socket = %self.inner.socket, "docker connection lost");
                *slot = None;
            }
        }
    }

    pub fn set_self_id(&self, id: Option<String>) {
        if let Ok(mut slot) = self.inner.self_id.write() {
            *slot = id;
        }
    }

    fn is_self(&self, id: &str) -> bool {
        self.inner.self_id
            .read()
            .ok()
            .and_then(|s| s.clone())
            .is_some_and(|own| own.starts_with(id) || id.starts_with(&own))
    }

    fn guard_not_self(&self, id: &str, action: &str) -> Result<(), AgentError> {
        if self.is_self(id) {
            return Err(
                AgentError::Forbidden(
                    format!("refusing to {action} the helion-agent container itself")
                )
            );
        }
        Ok(())
    }

    // --- actions ------------------------------------------------------------

    pub async fn start(&self, id: &str) -> Result<ContainerActionResult, AgentError> {
        let docker = self.require()?;
        docker
            .start_container(id, None::<StartContainerOptions<String>>).await
            .map_err(|e| from_docker("start", id, e))?;
        self.after_action(&docker, id, ContainerAction::Start).await
    }

    pub async fn stop(
        &self,
        id: &str,
        timeout_secs: Option<u32>
    ) -> Result<ContainerActionResult, AgentError> {
        self.guard_not_self(id, "stop")?;
        let docker = self.require()?;
        let options = timeout_secs.map(|t| StopContainerOptions { t: t as i64 });
        docker
            .stop_container(id, options).await
            .map_err(|e| from_docker("stop", id, e))?;
        self.after_action(&docker, id, ContainerAction::Stop).await
    }

    pub async fn restart(
        &self,
        id: &str,
        timeout_secs: Option<u32>
    ) -> Result<ContainerActionResult, AgentError> {
        self.guard_not_self(id, "restart")?;
        let docker = self.require()?;
        let options = timeout_secs.map(|t| RestartContainerOptions { t: t as isize });
        docker
            .restart_container(id, options).await
            .map_err(|e| from_docker("restart", id, e))?;
        self.after_action(&docker, id, ContainerAction::Restart).await
    }

    pub async fn remove(
        &self,
        id: &str,
        force: bool,
        volumes: bool
    ) -> Result<ContainerActionResult, AgentError> {
        self.guard_not_self(id, "remove")?;
        let docker = self.require()?;
        docker
            .remove_container(
                id,
                Some(RemoveContainerOptions { v: volumes, force, link: false })
            ).await
            .map_err(|e| from_docker("remove", id, e))?;

        // Volumes too: their in-use state changes, and `v` deletes some.
        self.poke_containers();
        self.poke_volumes();
        Ok(ContainerActionResult {
            id: id.to_string(),
            action: ContainerAction::Remove,
            state: None,
        })
    }

    /// Reads back the resulting state so the client can update without
    /// waiting for the next sample, and wakes the sampler.
    async fn after_action(
        &self,
        docker: &Docker,
        id: &str,
        action: ContainerAction
    ) -> Result<ContainerActionResult, AgentError> {
        self.poke_containers();

        let state = docker
            .inspect_container(id, None).await
            .ok()
            .and_then(|c| c.state)
            .and_then(|s| s.status)
            .map(|s| s.to_string().to_ascii_lowercase());

        Ok(ContainerActionResult { id: id.to_string(), action, state })
    }

    /// Removes a named volume. Docker refuses while a container still
    /// mounts it, which surfaces as a 409 rather than silently destroying
    /// data a running service depends on.
    pub async fn remove_volume(&self, name: &str, force: bool) -> Result<(), AgentError> {
        let docker = self.require()?;
        docker
            .remove_volume(name, Some(bollard::volume::RemoveVolumeOptions { force })).await
            .map_err(|e| from_docker("remove volume", name, e))?;

        self.poke_volumes();
        Ok(())
    }

    // --- logs ---------------------------------------------------------------

    pub fn logs_options(tail: String, since: i64, follow: bool) -> LogsOptions<String> {
        LogsOptions {
            follow,
            stdout: true,
            stderr: true,
            since,
            until: 0,
            timestamps: true,
            tail,
        }
    }

    /// Confirms the container exists before a log stream is opened, so the
    /// caller gets a clean 404 rather than an empty websocket.
    pub async fn assert_exists(&self, id: &str) -> Result<(), AgentError> {
        let docker = self.require()?;
        docker
            .inspect_container(id, None).await
            .map(|_| ())
            .map_err(|e| from_docker("inspect", id, e))
    }
}

/// Decides when failing Docker calls mean a stale socket rather than a Docker
/// that is restarting.
///
/// Compose bind-mounts the single file `/var/run/docker.sock`. When dockerd
/// restarts (an upgrade, with live-restore keeping containers up) it creates
/// a new socket, but the container keeps the old, deleted inode at the same
/// path, so every reconnect builds a client on a dead socket, forever. Only a
/// new mount, which means a new container start, fixes that.
///
/// So: calls failing for [`STALE_SOCKET_AFTER`], at least
/// [`STALE_SOCKET_MIN_FAILURES`] times, while the socket path exists. A
/// missing socket is Docker being stopped, and resets the streak: the
/// sampler keeps quietly retrying, as it always has. The window also bounds
/// how often a Docker that is genuinely broken can restart the agent.
#[derive(Debug, Default)]
pub struct FailureStreak {
    since: Option<Instant>,
    failures: u32,
}

impl FailureStreak {
    /// Records a failed call. True when it's time to give up.
    pub fn fail(&mut self, now: Instant, socket_exists: bool) -> bool {
        if !socket_exists {
            self.reset();
            return false;
        }
        let since = *self.since.get_or_insert(now);
        self.failures = self.failures.saturating_add(1);
        self.failures >= STALE_SOCKET_MIN_FAILURES &&
            now.saturating_duration_since(since) >= STALE_SOCKET_AFTER
    }

    /// A call succeeded, or the socket is gone.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn connect(socket: &str) -> Option<Docker> {
    match
        Docker::connect_with_socket(
            socket,
            120,
            bollard::API_DEFAULT_VERSION
        )
    {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::debug!(socket = %socket, error = %e, "docker connect failed");
            None
        }
    }
}

/// Finds the agent's own container id.
///
/// In Docker a container's hostname defaults to its short id, so matching the
/// hostname against the container list identifies us in the common case. If
/// the hostname was overridden, or we're not in a container at all, this
/// simply finds nothing and self-protection is inactive — which is correct,
/// because there's no own-container to protect.
pub async fn resolve_self_id(docker: &Docker, hostname: &str) -> Option<String> {
    if hostname.is_empty() {
        return None;
    }
    let containers = docker
        .list_containers(
            Some(ListContainersOptions::<String> { all: false, ..Default::default() })
        ).await
        .ok()?;

    containers
        .into_iter()
        .filter_map(|c| c.id)
        .find(|id| id.starts_with(hostname))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn gives_up_after_a_minute_of_failures_with_the_socket_present() {
        let t0 = Instant::now();
        let mut streak = FailureStreak::default();
        for i in 0..30 {
            assert!(!streak.fail(t0 + secs(i * 2), true), "tick {i}");
        }
        assert!(streak.fail(t0 + secs(60), true));
    }

    #[test]
    fn a_success_starts_the_window_again() {
        let t0 = Instant::now();
        let mut streak = FailureStreak::default();
        for i in 0..25 {
            streak.fail(t0 + secs(i * 2), true);
        }
        streak.reset();
        assert!(!streak.fail(t0 + secs(60), true));
        for i in 31..60 {
            assert!(!streak.fail(t0 + secs(i * 2), true), "tick {i}");
        }
        assert!(streak.fail(t0 + secs(120), true));
    }

    #[test]
    fn a_missing_socket_is_docker_stopped_and_never_gives_up() {
        let t0 = Instant::now();
        let mut streak = FailureStreak::default();
        for i in 0..100 {
            assert!(!streak.fail(t0 + secs(i * 2), false));
        }
        // Coming back counts from then, not from the first failure.
        for i in 100..130 {
            assert!(!streak.fail(t0 + secs(i * 2), true), "tick {i}");
        }
        assert!(streak.fail(t0 + secs(260), true));
    }

    #[test]
    fn a_few_slow_failures_are_not_enough() {
        let t0 = Instant::now();
        let mut streak = FailureStreak::default();
        for i in 0..STALE_SOCKET_MIN_FAILURES - 1 {
            assert!(!streak.fail(t0 + secs(u64::from(i) * 120), true));
        }
        assert!(streak.fail(t0 + secs(600), true));
    }
}
