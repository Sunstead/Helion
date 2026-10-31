use serde::{ Deserialize, Serialize };
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Tcp,
    Udp,
    Sctp,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct PortInfo {
    pub ip: Option<String>,
    pub private_port: u16,
    pub public_port: Option<u16>,
    pub port_type: Option<PortType>,
}

/// A container's health check result, for containers that have one.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum ContainerHealth {
    Starting,
    Healthy,
    Unhealthy,
}

impl ContainerHealth {
    /// From the suffix Docker puts on its status string: "Up 3 hours
    /// (healthy)", "(unhealthy)", "(health: starting)".
    pub fn from_status(status: &str) -> Option<Self> {
        if status.ends_with("(healthy)") {
            Some(Self::Healthy)
        } else if status.ends_with("(unhealthy)") {
            Some(Self::Unhealthy)
        } else if status.ends_with("(health: starting)") {
            Some(Self::Starting)
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    /// Docker's human string, e.g. "Up 3 hours (healthy)".
    pub status: String,
    /// Docker's machine state, e.g. "running", "exited".
    pub state: String,
    /// `None` when the container has no health check. Absent from agents
    /// before 0.5.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub health: Option<ContainerHealth>,
    pub ports: Vec<PortInfo>,
    /// RFC3339. Cached against state transitions rather than re-inspected.
    pub started_at: Option<String>,
    #[ts(type = "number")]
    pub created_unix: i64,
    pub restart_count: u32,
    pub compose_project: Option<String>,
    /// The service name in compose. Absent from agents before 0.8.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub compose_service: Option<String>,
    pub helion_service: Option<String>,
    pub helion_service_description: Option<String>,
    pub helion_service_url: Option<String>,
    /// The name of this container's icon, fetchable from `/v1/icons/<name>`.
    /// Set only once the agent holds the file. Absent from agents before 0.15.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub icon: Option<String>,
    /// The `helion.update*` labels, for the agent's update rules only.
    #[serde(skip)]
    #[ts(skip)]
    pub update_labels: std::collections::BTreeMap<String, String>,
    /// Percent of one core x online cores, differenced across agent ticks.
    /// 0.0 on a container's first sighting; correct from the next tick.
    pub cpu_pct: f32,
    #[ts(type = "number")]
    pub mem_used_bytes: u64,
    /// 0 when the container has no memory limit set.
    #[ts(type = "number")]
    pub mem_limit_bytes: u64,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainersResponse {
    pub containers: Vec<ContainerInfo>,
    /// Unix seconds of the sample these came from.
    #[ts(type = "number")]
    pub sampled_at: i64,
    /// Docker isn't answering: `containers` is the last list it gave, or
    /// empty if it never has. Absent from 0.8.0 and older agents.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub docker_unavailable: bool,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct VolumeInfo {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: Option<String>,
    pub scope: Option<String>,
    pub compose_project: Option<String>,
    pub helion_service: Option<String>,
    /// Names of containers currently mounting this volume. Empty = unused.
    pub in_use_by: Vec<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct VolumesResponse {
    pub volumes: Vec<VolumeInfo>,
    #[ts(type = "number")]
    pub sampled_at: i64,
}

// --- actions ---------------------------------------------------------------

#[derive(Serialize, Deserialize, TS, Debug, Clone, Default)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainerActionRequest {
    /// Seconds to wait for graceful shutdown before SIGKILL. Docker's default
    /// is 10 when omitted.
    pub timeout_secs: Option<u32>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainerActionResult {
    pub id: String,
    pub action: ContainerAction,
    /// State Docker reported after the action, when it could be read.
    pub state: Option<String>,
}

// --- logs ------------------------------------------------------------------

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct LogLine {
    pub stream: LogStream,
    /// RFC3339 from Docker when timestamps are enabled.
    pub ts: Option<String>,
    pub text: String,
    /// The container's ID, set only on the all-containers streams, where
    /// lines from every container share one connection.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub container: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct LogsResponse {
    pub id: String,
    pub lines: Vec<LogLine>,
}

/// Frames the log WebSocket sends. Tagged so the client can switch on `type`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogFrame {
    Line(LogLine),
    /// Lines were dropped. No current agent sends this: its sends are
    /// awaited, so a slow client slows the Docker read instead. Kept so the
    /// wire format stays stable.
    Truncated {
        dropped: u32,
    },
    /// The container exited, the stream ended or the agent is stopping; no
    /// more lines are coming.
    Closed {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::ContainerHealth;

    #[test]
    fn health_comes_from_the_status_suffix() {
        assert_eq!(ContainerHealth::from_status("Up 3 hours (healthy)"), Some(ContainerHealth::Healthy));
        assert_eq!(ContainerHealth::from_status("Up 2 minutes (unhealthy)"), Some(ContainerHealth::Unhealthy));
        assert_eq!(ContainerHealth::from_status("Up 5 seconds (health: starting)"), Some(ContainerHealth::Starting));
        assert_eq!(ContainerHealth::from_status("Up 3 hours"), None);
        assert_eq!(ContainerHealth::from_status("Exited (1) 2 minutes ago"), None);
    }
}
