//! Shared application state.
//!
//! Note what is *not* here: no `Mutex` around a `System`, no per-request
//! Docker client, no rate snapshots. Handlers only ever read the latest
//! published value, so the request path does no sampling and takes no locks.

use crate::{
    auth::Auth,
    backups::BackupSnapshot,
    config::Config,
    docker::DockerHandle,
    icons::IconsHandle,
    events::EventsHandle,
    notify::NotifyHandle,
    peers::PeersHandle,
    history::HistoryHandle,
    shutdown::Shutdown,
    sample::{
        docker::{ ContainerSnapshot, VolumeSnapshot },
        facts::HostFacts,
        host::HostSnapshot,
        tailnet::TailnetSnapshot,
        wol::WolHandle,
    },
    updates::UpdatesHandle,
    uptime::UptimeHandle,
};
use helion_common::types::Capabilities;
use std::{ ops::Deref, sync::Arc };
use tokio::sync::watch;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub cfg: Arc<Config>,
    pub auth: Auth,
    pub facts: Arc<HostFacts>,
    pub docker: DockerHandle,
    pub history: Option<HistoryHandle>,

    pub host_rx: watch::Receiver<Arc<HostSnapshot>>,
    pub containers_rx: watch::Receiver<Arc<ContainerSnapshot>>,
    pub volumes_rx: watch::Receiver<Arc<VolumeSnapshot>>,
    pub backups_rx: Option<watch::Receiver<Arc<BackupSnapshot>>>,
    /// Rereads the backup status now, as after a request.
    pub backups_poke: Option<Arc<tokio::sync::Notify>>,
    pub tailnet_rx: Option<watch::Receiver<Arc<TailnetSnapshot>>>,
    pub wol: Option<WolHandle>,
    pub events: Option<EventsHandle>,
    pub notify: Option<NotifyHandle>,
    pub uptime: Option<UptimeHandle>,
    pub updates: Option<UpdatesHandle>,
    pub peers: Option<PeersHandle>,
    pub icons: Option<IconsHandle>,
    /// Ends streams and log sockets when the agent stops.
    pub shutdown: Shutdown,
}

impl AppState {
    pub fn new(inner: Inner) -> Self {
        Self(Arc::new(inner))
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            host_metrics: true,
            containers: true,
            container_actions: self.cfg.allow_actions,
            container_logs: self.cfg.docker.allow_logs,
            websocket_logs: self.cfg.docker.allow_logs,
            volumes: true,
            volume_actions: self.cfg.allow_actions,
            metrics_history: self.history.is_some(),
            backups: self.backups_rx.is_some(),
            tailnet: self.tailnet_rx.is_some(),
            wol: self.wol.is_some(),
            wol_actions: self.wol.is_some() && self.cfg.allow_actions,
            volume_stream: true,
            all_logs: self.cfg.docker.allow_logs,
            events: self.events.is_some(),
            notify: self.notify.is_some(),
            notify_actions: self.notify.is_some() && self.cfg.allow_actions,
            uptime: self.uptime.is_some(),
            uptime_actions: self.uptime.is_some() && self.cfg.allow_actions,
            backup_actions: self.backups_rx.is_some() &&
            self.cfg.backups.requests_dir.is_some() &&
            self.cfg.allow_actions,
            updates: self.updates.is_some(),
            update_actions: self.updates.as_ref().is_some_and(|u| u.can_apply) && self.cfg.allow_actions,
            peers: self.peers.is_some(),
        }
    }
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
