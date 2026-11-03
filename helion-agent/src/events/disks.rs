//! Filesystems filling up. Each level opens at its threshold and clears
//! [`HYSTERESIS`] points below it, so a disk hovering at the line doesn't
//! flap.

use super::{ human_bytes, EventsHandle, ProblemSpec, Report, Resolution };
use crate::sample::host::HostSnapshot;
use helion_common::types::{ DiskInfo, EventCategory, Problem, Severity };
use std::{ sync::Arc, time::Duration };
use tokio::sync::watch;

const HYSTERESIS: f64 = 5.0;
const EVERY: Duration = Duration::from_secs(60);

struct Level {
    kind: &'static str,
    severity: Severity,
    pct: f64,
}

fn key(label: &str, kind: &str) -> String {
    format!("disk:{label}:{kind}")
}

pub fn observe(disks: &[DiskInfo], open: &[Problem], warn_pct: u8, critical_pct: u8) -> Vec<Report> {
    let levels = [
        Level { kind: "disk_warning", severity: Severity::Warning, pct: f64::from(warn_pct) },
        Level { kind: "disk_critical", severity: Severity::Error, pct: f64::from(critical_pct) },
    ];
    let mut out = Vec::new();

    for d in disks.iter().filter(|d| d.total_bytes > 0) {
        let pct = (d.used_bytes as f64) * 100.0 / (d.total_bytes as f64);
        for level in levels.iter().filter(|l| pct >= l.pct) {
            out.push(
                Report::Open(ProblemSpec {
                    key: key(&d.label, level.kind),
                    category: EventCategory::Disk,
                    kind: level.kind,
                    severity: level.severity,
                    subject: d.label.clone(),
                    title: format!("{} is {pct:.0}% full", d.label),
                    detail: Some(
                        format!(
                            "{} of {} used, {} free.",
                            human_bytes(d.used_bytes),
                            human_bytes(d.total_bytes),
                            human_bytes(d.total_bytes.saturating_sub(d.used_bytes))
                        )
                    ),
                    service: None,
                    depends_on: None,
                })
            );
        }
    }

    for p in open.iter().filter(|p| p.category == EventCategory::Disk) {
        let Some(level) = levels.iter().find(|l| l.kind == p.kind) else {
            continue;
        };
        let clear_below = level.pct - HYSTERESIS;
        let title = match disks.iter().find(|d| d.label == p.subject) {
            None => format!("{} is no longer mounted", p.subject),
            Some(d) if d.total_bytes > 0 => {
                let pct = (d.used_bytes as f64) * 100.0 / (d.total_bytes as f64);
                if pct >= clear_below {
                    continue;
                }
                format!("{} is down to {pct:.0}% full", p.subject)
            }
            Some(_) => continue,
        };
        out.push(Report::Resolve(Resolution { key: p.key.clone(), title, detail: None }));
    }
    out
}

pub fn spawn(events: EventsHandle, host: watch::Receiver<Arc<HostSnapshot>>, warn_pct: u8, critical_pct: u8) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let disks = host.borrow().disks.clone();
            events.report_all(observe(&disks, &events.open_problems(), warn_pct, critical_pct));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use helion_common::types::DiskKind;

    fn disk(label: &str, pct: u64) -> DiskInfo {
        DiskInfo {
            mount: label.into(),
            label: label.into(),
            used_bytes: pct * 4_180_000_000,
            total_bytes: 418_000_000_000,
            read_bps: 0.0,
            write_bps: 0.0,
            io_available: true,
            kind: DiskKind::Ssd,
        }
    }

    fn open(label: &str, kind: &str) -> Problem {
        Problem {
            key: key(label, kind),
            category: EventCategory::Disk,
            kind: kind.into(),
            severity: Severity::Warning,
            subject: label.into(),
            title: String::new(),
            detail: None,
            service: None,
            opened_at: 0,
            event_id: 1,
            depends_on: None,
            acked_by: None,
            acked_at: None,
        }
    }

    fn keys(out: &[Report]) -> Vec<String> {
        out.iter()
            .map(|r| {
                match r {
                    Report::Open(p) => format!("open {}", p.key),
                    Report::Resolve(r) => format!("resolve {}", r.key),
                    Report::Event(e) => format!("event {}", e.kind),
                }
            })
            .collect()
    }

    #[test]
    fn levels_open_at_their_thresholds() {
        assert!(observe(&[disk("/", 84)], &[], 85, 95).is_empty());
        assert_eq!(keys(&observe(&[disk("/", 86)], &[], 85, 95)), ["open disk:/:disk_warning"]);
        let out = observe(&[disk("/", 96)], &[], 85, 95);
        assert_eq!(keys(&out), ["open disk:/:disk_warning", "open disk:/:disk_critical"]);
        let Report::Open(p) = &out[1] else { unreachable!() };
        assert_eq!(p.title, "/ is 96% full");
        assert_eq!(p.detail.as_deref(), Some("401 GB of 418 GB used, 16.7 GB free."));
        assert_eq!(p.severity, Severity::Error);
    }

    #[test]
    fn a_level_clears_only_well_below_its_threshold() {
        let open = [open("/", "disk_warning")];
        assert!(keys(&observe(&[disk("/", 82)], &open, 85, 95)).is_empty(), "hovering near the line");
        assert_eq!(keys(&observe(&[disk("/", 79)], &open, 85, 95)), ["resolve disk:/:disk_warning"]);
    }

    #[test]
    fn a_disk_that_disappears_resolves() {
        let open = [open("/srv/backups", "disk_critical")];
        let out = observe(&[disk("/", 50)], &open, 85, 95);
        let [Report::Resolve(r)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(r.title, "/srv/backups is no longer mounted");
    }
}
