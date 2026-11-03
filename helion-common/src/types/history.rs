use serde::{ Deserialize, Serialize };
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum MetricStep {
    /// Let the agent pick the coarsest tier that fits within `max_points`.
    Auto,
    #[serde(rename = "1s")]
    #[ts(rename = "1s")]
    OneSec,
    #[serde(rename = "1m")]
    #[ts(rename = "1m")]
    OneMin,
    #[serde(rename = "5m")]
    #[ts(rename = "5m")]
    FiveMin,
}

/// Columnar on purpose: at 10k points, parallel arrays are roughly 4x smaller
/// than an array of objects because each key name is emitted once instead of
/// ten thousand times. Zipping into a chart library's row shape is a few lines
/// on the client.
///
/// Every `Vec` here has the same length as `ts`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct MetricSeries {
    pub step_secs: u32,
    #[ts(type = "number")]
    pub from: i64,
    #[ts(type = "number")]
    pub to: i64,
    /// Unix seconds, ascending, one per bucket.
    #[ts(type = "number[]")]
    pub ts: Vec<i64>,
    pub cpu_pct: Vec<f32>,
    /// Peak within the bucket, so a zoomed-out chart can't hide a spike.
    /// Equal to `cpu_pct` on the raw 1s tier.
    pub cpu_pct_max: Vec<f32>,
    pub mem_used_bytes: Vec<f64>,
    pub swap_used_bytes: Vec<f64>,
    pub net_rx_bps: Vec<f64>,
    pub net_rx_bps_max: Vec<f64>,
    pub net_tx_bps: Vec<f64>,
    pub net_tx_bps_max: Vec<f64>,
    pub disk_read_bps: Vec<f64>,
    pub disk_read_bps_max: Vec<f64>,
    pub disk_write_bps: Vec<f64>,
    pub disk_write_bps_max: Vec<f64>,
    pub load1: Vec<f64>,
    /// Samples that went into each bucket. 0 means a gap the UI should break
    /// the line across rather than interpolate through.
    pub n: Vec<u32>,
}

/// What `top` ranks containers by in `/v1/metrics/containers`.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum ContainerRank {
    Cpu,
    Mem,
}

/// Per-container history, columnar like [`MetricSeries`] but with one shared
/// time axis: every container's arrays have the length of `ts`, and `null`
/// where it has no bucket (not running, or not yet created). That is the shape
/// a multi-line chart wants, with no client-side merge.
///
/// Containers are keyed by name, which survives a Compose recreate; the ID
/// does not. Only the 1m and 5m tiers exist: the live table covers the rest.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainerSeries {
    pub step_secs: u32,
    #[ts(type = "number")]
    pub from: i64,
    #[ts(type = "number")]
    pub to: i64,
    #[ts(type = "number[]")]
    pub ts: Vec<i64>,
    pub containers: Vec<ContainerHistory>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ContainerHistory {
    pub name: String,
    /// The `helion.service` it had when last seen.
    pub service: Option<String>,
    pub cpu_pct: Vec<Option<f64>>,
    pub cpu_pct_max: Vec<Option<f64>>,
    pub mem_used_bytes: Vec<Option<f64>>,
    pub mem_used_bytes_max: Vec<Option<f64>>,
    pub net_rx_bps: Vec<Option<f64>>,
    pub net_tx_bps: Vec<Option<f64>>,
    pub disk_read_bps: Vec<Option<f64>>,
    pub disk_write_bps: Vec<Option<f64>>,
}
