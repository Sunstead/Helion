use serde::{ Deserialize, Serialize };
use ts_rs::TS;

/// Where an expected service stands now.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum ExpectedState {
    Running,
    /// None of its containers is running, and nobody stopped it through Helion.
    Missing,
    /// Stopped or removed through Helion, so its absence is no problem.
    Stopped,
}

/// A `helion.service` that has run long enough to be expected, so its
/// disappearing is reported. Learned from what runs; forgetting one is the
/// only edit.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ExpectedService {
    pub service: String,
    pub state: ExpectedState,
    /// Unix seconds.
    #[ts(type = "number")]
    pub first_seen: i64,
    /// When one of its containers was last seen running, unix seconds.
    #[ts(type = "number")]
    pub last_running: i64,
    /// Whether any of its containers exist, running or not.
    pub present: bool,
    /// Who stopped it through Helion, when `state` is `stopped`.
    pub stopped_by: Option<String>,
    #[ts(type = "number | null")]
    pub stopped_at: Option<i64>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ExpectedServicesResponse {
    pub services: Vec<ExpectedService>,
}
