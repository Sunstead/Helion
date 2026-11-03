//! Drive health, read from a file the host writes.
//!
//! Like backups, the agent runs nothing: reading S.M.A.R.T. needs raw access
//! to the block devices, which is root on the host. A timer there runs
//! `smartctl` and writes a small JSON file (Jupiter's
//! `scripts/helion-disk-health.sh`); the agent mounts it read-only and polls
//! it. The file is tiny and changes hourly, so each poll just reads it.

use crate::{ backups::{ format_rfc3339, parse_rfc3339 }, config::HardwareConfig, sample::host::unix_now };
use helion_common::types::{ DiskHealth, DiskHealthStatus };
use serde::Deserialize;
use std::{ sync::Arc, time::Duration };
use tokio::sync::watch;

const POLL: Duration = Duration::from_secs(300);
/// Stale after this many missed intervals.
const STALE_AFTER: u64 = 3;
/// Anything bigger isn't the script's.
const MAX_FILE: u64 = 1024 * 1024;

/// The file's shape. Only `generated_at` is required.
#[derive(Deserialize)]
struct StatusFile {
    generated_at: String,
    #[serde(default)]
    disks: Vec<DiskHealth>,
}

pub struct DiskHealthSnapshot {
    pub json: Arc<str>,
    pub status: Arc<DiskHealthStatus>,
}

impl DiskHealthSnapshot {
    fn new(status: DiskHealthStatus) -> Self {
        let json = serde_json::to_string(&status).unwrap_or_else(|_| "{}".into()).into();
        Self { json, status: Arc::new(status) }
    }
}

/// Parses the file's contents. Pure, so the staleness rule is testable.
pub fn parse(contents: Option<&str>, interval_secs: u64, now: i64) -> DiskHealthStatus {
    let file = contents.and_then(|c| serde_json::from_str::<StatusFile>(c).ok());
    let Some(file) = file else {
        return DiskHealthStatus { generated_at: String::new(), stale: true, disks: Vec::new() };
    };
    let generated = parse_rfc3339(&file.generated_at);
    let limit = (interval_secs.max(60) * STALE_AFTER) as i64;
    DiskHealthStatus {
        generated_at: generated.map(format_rfc3339).unwrap_or(file.generated_at),
        stale: generated.is_none_or(|g| now - g > limit),
        disks: file.disks,
    }
}

fn read(cfg: &HardwareConfig) -> DiskHealthStatus {
    let contents = std::fs
        ::metadata(&cfg.disk_health_file)
        .ok()
        .filter(|m| m.len() <= MAX_FILE)
        .and_then(|_| std::fs::read_to_string(&cfg.disk_health_file).ok());
    parse(contents.as_deref(), cfg.expected_interval_secs, unix_now())
}

pub fn spawn(cfg: &HardwareConfig) -> watch::Receiver<Arc<DiskHealthSnapshot>> {
    let cfg = cfg.clone();
    let (tx, rx) = watch::channel(Arc::new(DiskHealthSnapshot::new(read(&cfg))));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            let status = tokio::task::spawn_blocking({
                let cfg = cfg.clone();
                move || read(&cfg)
            }).await;
            let Ok(status) = status else {
                continue;
            };
            if *tx.borrow().status != status && tx.send(Arc::new(DiskHealthSnapshot::new(status))).is_err() {
                return;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;

    #[test]
    fn a_fresh_file_lists_its_disks() {
        let file = format!(
            r#"{{"generated_at":"{}","disks":[{{"device":"/dev/sda","serial":"W1","passed":true,"pending":0}}]}}"#,
            format_rfc3339(NOW - 600)
        );
        let s = parse(Some(&file), 3_600, NOW);
        assert!(!s.stale);
        assert_eq!(s.disks[0].id(), "W1");
        assert_eq!(s.disks[0].pending, Some(0));
    }

    #[test]
    fn missing_unreadable_or_old_is_stale() {
        assert!(parse(None, 3_600, NOW).stale);
        assert!(parse(Some("not json"), 3_600, NOW).stale);
        let old = format!(r#"{{"generated_at":"{}"}}"#, format_rfc3339(NOW - 4 * 3_600));
        assert!(parse(Some(&old), 3_600, NOW).stale);
    }
}
