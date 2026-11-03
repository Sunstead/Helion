use serde::{ Deserialize, Serialize };
use ts_rs::TS;

/// One drive's S.M.A.R.T. summary, as the host's script reads it with
/// `smartctl`. Every figure is optional: drives report different subsets.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct DiskHealth {
    /// e.g. `/dev/sda`. Can change between boots; `serial` doesn't.
    pub device: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    /// `hdd`, `ssd` or `nvme`, as the script decides.
    #[serde(default)]
    pub kind: Option<String>,
    /// The drive's own overall health assessment.
    #[serde(default)]
    pub passed: Option<bool>,
    #[serde(default)]
    pub temp_c: Option<f32>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub power_on_hours: Option<u64>,
    /// Sectors the drive has moved elsewhere. Rising means it's wearing out.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub reallocated: Option<u64>,
    /// Sectors waiting to be reallocated: unreadable now.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub pending: Option<u64>,
    /// NVMe media and data integrity errors.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub media_errors: Option<u64>,
    /// NVMe wear, 100 meaning the rated endurance is used up.
    #[serde(default)]
    pub percent_used: Option<u8>,
}

impl DiskHealth {
    /// Stable across reboots when the drive reports a serial.
    pub fn id(&self) -> &str {
        self.serial.as_deref().unwrap_or(&self.device)
    }

    /// "/dev/sda (WDC WD20EZBX)", for titles.
    pub fn name(&self) -> String {
        match &self.model {
            Some(m) => format!("{} ({m})", self.device),
            None => self.device.clone(),
        }
    }
}

/// `GET /v1/hardware/disks`.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct DiskHealthStatus {
    /// RFC3339, when the host last read the drives; empty if never.
    pub generated_at: String,
    /// Not refreshed for three of its intervals, or unreadable.
    pub stale: bool,
    pub disks: Vec<DiskHealth>,
}
