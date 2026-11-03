use serde::{ Deserialize, Serialize };
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Ssd,
    Hdd,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct DiskInfo {
    /// Mount point as the agent sees it. Inside a container this is the bind
    /// path (`/host/rootfs`), which is why `label` exists.
    pub mount: String,
    /// What to show the user — the host-side path (`/`), from config.
    pub label: String,
    #[ts(type = "number")]
    pub used_bytes: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    pub read_bps: f64,
    pub write_bps: f64,
    /// False when the agent can't find this disk's I/O counters (a network
    /// or pooled filesystem with no block device of its own). Its rates are
    /// then 0 and mean nothing, so the UI shows n/a and totals leave it out.
    /// Agents before 0.10 don't send it (they always claimed counters), hence
    /// optional in TypeScript: only `false` means no I/O.
    #[serde(default = "io_available_default")]
    #[ts(as = "Option<bool>", optional)]
    pub io_available: bool,
    pub kind: DiskKind,
}

fn io_available_default() -> bool {
    true
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct NetInfo {
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    #[ts(type = "number")]
    pub rx_total_bytes: u64,
    #[ts(type = "number")]
    pub tx_total_bytes: u64,
}

/// One sample of a node's live state.
///
/// All rates are **bytes per second** and all sizes are **bytes**. The agent
/// ships raw units and the UI formats them; the previous mix of MB/s for disk
/// and megabits/s for network in the same struct was a unit-confusion bug
/// waiting to be hit.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct HostInfo {
    // --- static facts, memcpy'd from HostFacts rather than recomputed -------
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_physical_cores: u32,
    pub cpu_logical_cores: u32,
    #[ts(type = "number")]
    pub cpu_freq_mhz: u64,
    #[ts(type = "number")]
    pub mem_total_bytes: u64,
    #[ts(type = "number")]
    pub swap_total_bytes: u64,

    // --- live ---------------------------------------------------------------
    pub cpu_pct: f32,
    /// Per-core usage, same order as the kernel reports them.
    pub cpu_per_core: Vec<f32>,
    #[ts(type = "number")]
    pub mem_used_bytes: u64,
    #[ts(type = "number")]
    pub swap_used_bytes: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub disk: Vec<DiskInfo>,
    pub nets: Vec<NetInfo>,
    /// Sum across interfaces that survived the include/exclude filters.
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    #[ts(type = "number")]
    pub uptime_secs: u64,

    /// Unix seconds when this sample was taken.
    #[ts(type = "number")]
    pub sampled_at: i64,
    /// Monotonic per-agent counter. Lets the client detect a restarted agent
    /// (seq goes backwards) and dropped samples.
    #[ts(type = "number")]
    pub seq: u64,
    /// The host's temperature sensors, read every 10 s. Empty where the agent
    /// can't see any (no `/sys/class/hwmon`), and from agents before 0.17.
    #[serde(default)]
    pub temps: Vec<TempReading>,
}

/// One temperature sensor.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct TempReading {
    /// The sensor's name as the kernel gives it, e.g. "coretemp Package id 0".
    pub label: String,
    pub celsius: f32,
    /// Where the hardware itself calls it critical, when it says.
    pub critical: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::DiskInfo;

    #[test]
    fn disks_from_older_agents_count_as_having_io() {
        let d: DiskInfo = serde_json
            ::from_str(
                r#"{"mount":"/","label":"/","used_bytes":1,"total_bytes":2,"read_bps":0,"write_bps":0,"kind":"ssd"}"#
            )
            .unwrap();
        assert!(d.io_available);
    }
}
