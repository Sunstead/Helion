use serde::{ Deserialize, Serialize };
use ts_rs::TS;

/// Served unauthenticated from `/v1/info` so the add-node flow can distinguish
/// "nothing is listening here" from "an agent is here and wants a token"
/// before it has a token to present.
///
/// A 404 on this route means a pre-0.2 agent: assume `api_version: 0` and the
/// original four read-only endpoints.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct AgentInfo {
    pub agent_version: String,
    pub api_version: u32,
    pub auth_required: bool,
    /// `None` unless the request was authenticated — the node's name is not
    /// something an unauthenticated caller should be able to harvest.
    pub node_name: Option<String>,
    pub capabilities: Capabilities,
    /// Who the agent thinks is asking. `None` when unauthenticated, and
    /// absent from pre-0.3 agents.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub principal: Option<PrincipalInfo>,
    /// How to sign in. Absent from pre-0.3 agents, which used a shared token.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub auth: Option<AuthInfo>,
    /// What this node is for, in a few words ("Public edge"), from its
    /// config. `None` unless authenticated, like `node_name`; absent before
    /// 0.11.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub node_description: Option<String>,
}

/// Public sign-in parameters, so the app can start the provider's flow
/// before it has a token. None of this is secret.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../app/src/generated/")]
pub enum AuthInfo {
    /// `allow_anonymous`: no sign-in.
    None,
    Oidc {
        issuer: String,
        client_id: String,
        scopes: String,
    },
}

/// The authenticated caller, as far as the UI needs to know. Action buttons
/// need `admin` as well as the matching capability.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct PrincipalInfo {
    pub name: String,
    pub admin: bool,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct Capabilities {
    pub host_metrics: bool,
    pub containers: bool,
    pub container_actions: bool,
    pub container_logs: bool,
    pub websocket_logs: bool,
    pub volumes: bool,
    pub volume_actions: bool,
    pub metrics_history: bool,
    pub backups: bool,
    /// `/v1/tailnet`. Absent from agents before 0.3.
    #[serde(default)]
    pub tailnet: bool,
    /// `/v1/wol`. Absent from agents before 0.3.
    #[serde(default)]
    pub wol: bool,
    /// Wake and edit targets: `wol` plus `allow_actions`.
    #[serde(default)]
    pub wol_actions: bool,
    /// `/v1/volumes/stream`. Absent from agents before 0.4, which the app
    /// polls instead.
    #[serde(default)]
    pub volume_stream: bool,
    /// `/v1/logs` and `/v1/logs/ws`: every running container's logs at once.
    /// Absent from agents before 0.4.
    #[serde(default)]
    pub all_logs: bool,
    /// `/v1/events`: the node's event log and open problems. Absent from
    /// agents before 0.5.
    #[serde(default)]
    pub events: bool,
    /// `/v1/notify`: notification channels (admins only). Absent from agents
    /// before 0.5.
    #[serde(default)]
    pub notify: bool,
    /// Editing channels: `notify` plus `allow_actions`.
    #[serde(default)]
    pub notify_actions: bool,
    /// `/v1/uptime`: HTTP and TCP checks. Absent from agents before 0.6.
    #[serde(default)]
    pub uptime: bool,
    /// Adding and editing checks: `uptime` plus `allow_actions`.
    #[serde(default)]
    pub uptime_actions: bool,
    /// Starting a backup or a restore test: `backups` with a requests
    /// directory, plus `allow_actions`. Absent from agents before 0.7.
    #[serde(default)]
    pub backup_actions: bool,
    /// `/v1/updates`: newer image tags. Absent from agents before 0.8.
    #[serde(default)]
    pub updates: bool,
    /// Applying updates: `updates` with a GitHub token and repository, plus
    /// `allow_actions`.
    #[serde(default)]
    pub update_actions: bool,
    /// `/v1/peers`: other agents this one exchanges heartbeats with. Absent
    /// from agents before 0.11.
    #[serde(default)]
    pub peers: bool,
    /// `/v1/metrics/containers`: per-container history. Absent from agents
    /// before 0.15.
    #[serde(default)]
    pub container_history: bool,
    /// `/v1/services/expected`: services the agent expects to be running, so
    /// one that disappears is reported. Absent from agents before 0.15.
    #[serde(default)]
    pub expected_services: bool,
    /// Acknowledging problems and silencing notifications: `events` plus
    /// `allow_actions`. Absent from agents before 0.16.
    #[serde(default)]
    pub alert_actions: bool,
    /// `/v1/events/:id/logs`: a crash's last lines kept with its event.
    /// Absent from agents before 0.16.
    #[serde(default)]
    pub event_logs: bool,
    /// `/v1/hardware/disks`: S.M.A.R.T. from the host's status file. Absent
    /// from agents before 0.17.
    #[serde(default)]
    pub disk_health: bool,
}

impl Capabilities {
    /// What a pre-0.2 agent (no `/v1/info` route) is assumed to support.
    pub fn legacy() -> Self {
        Self {
            host_metrics: true,
            containers: true,
            container_actions: false,
            container_logs: false,
            websocket_logs: false,
            volumes: true,
            volume_actions: false,
            metrics_history: false,
            backups: false,
            tailnet: false,
            wol: false,
            wol_actions: false,
            volume_stream: false,
            all_logs: false,
            events: false,
            notify: false,
            notify_actions: false,
            uptime: false,
            uptime_actions: false,
            backup_actions: false,
            updates: false,
            update_actions: false,
            peers: false,
            container_history: false,
            expected_services: false,
            alert_actions: false,
            event_logs: false,
            disk_health: false,
        }
    }
}
