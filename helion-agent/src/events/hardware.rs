//! Drive health from the host's S.M.A.R.T. file.
//!
//! - A drive whose own assessment fails opens an error (`smart:<id>:failed`).
//! - Pending sectors (unreadable now) open a warning while there are any.
//! - NVMe wear at 90% or more opens a warning.
//! - Reallocated sectors or media errors going up is an event each time: the
//!   count never goes down, so it can't be a problem that clears.
//! - A file that stops updating opens `smart:stale`.
//!
//! Keyed by serial, so a drive that moves from `/dev/sda` to `/dev/sdb` is the
//! same drive. A drive that's gone resolves its problems: it was replaced.
//! Pure, apart from the counts it remembers to notice a rise.

use super::{ EventsHandle, NewEvent, ProblemSpec, Report, Resolution };
use crate::hardware::DiskHealthSnapshot;
use helion_common::types::{ DiskHealth, DiskHealthStatus, EventCategory, Problem, Severity };
use std::{ collections::HashMap, sync::Arc };
use tokio::sync::watch;

const WORN_PCT: u8 = 90;
const STALE: &str = "smart:stale";

fn key(id: &str, kind: &str) -> String {
    format!("smart:{id}:{kind}")
}

/// Reallocated sectors and media errors, last seen.
#[derive(Debug, Default)]
pub struct DriveTracker {
    counts: HashMap<String, (Option<u64>, Option<u64>)>,
}

fn spec(d: &DiskHealth, kind: &'static str, severity: Severity, title: String, detail: String) -> Report {
    Report::Open(ProblemSpec {
        key: key(d.id(), kind),
        category: EventCategory::Disk,
        kind,
        severity,
        subject: d.name(),
        title,
        detail: Some(detail),
        service: None,
        depends_on: None,
    })
}

impl DriveTracker {
    pub fn observe(&mut self, status: &DiskHealthStatus, open: &[Problem]) -> Vec<Report> {
        let mut out = Vec::new();
        let is_open = |k: &str| open.iter().any(|p| p.key == k);

        if status.stale {
            out.push(
                Report::Open(ProblemSpec {
                    key: STALE.into(),
                    category: EventCategory::Disk,
                    kind: "smart_stale",
                    severity: Severity::Warning,
                    subject: "Drive health".into(),
                    title: "Drive health isn't being reported".into(),
                    detail: Some(
                        "The host's disk health file is missing or hasn't been updated for three of its intervals.".into()
                    ),
                    service: None,
                    depends_on: None,
                })
            );
            // Nothing below is current; leave the drives' problems as they are.
            return out;
        }
        if is_open(STALE) {
            out.push(
                Report::Resolve(Resolution { key: STALE.into(), title: "Drive health is reported again".into(), detail: None })
            );
        }

        for d in &status.disks {
            let name = d.name();
            match d.passed {
                Some(false) =>
                    out.push(
                        spec(
                            d,
                            "smart_failed",
                            Severity::Error,
                            format!("{name} failed its health check"),
                            "The drive itself predicts failure. Replace it, and check that its data is backed up.".into()
                        )
                    ),
                Some(true) if is_open(&key(d.id(), "smart_failed")) =>
                    out.push(
                        Report::Resolve(Resolution {
                            key: key(d.id(), "smart_failed"),
                            title: format!("{name} passes its health check"),
                            detail: None,
                        })
                    ),
                _ => {}
            }

            match d.pending {
                Some(n) if n > 0 =>
                    out.push(
                        spec(
                            d,
                            "smart_pending",
                            Severity::Warning,
                            format!("{name} has {n} unreadable sectors"),
                            "Sectors waiting to be reallocated. Data on them may be lost.".into()
                        )
                    ),
                Some(0) if is_open(&key(d.id(), "smart_pending")) =>
                    out.push(
                        Report::Resolve(Resolution {
                            key: key(d.id(), "smart_pending"),
                            title: format!("{name} has no unreadable sectors"),
                            detail: None,
                        })
                    ),
                _ => {}
            }

            if let Some(used) = d.percent_used.filter(|u| *u >= WORN_PCT) {
                out.push(
                    spec(
                        d,
                        "smart_worn",
                        Severity::Warning,
                        format!("{name} is {used}% worn"),
                        "It has used most of its rated write endurance.".into()
                    )
                );
            }

            let now = (d.reallocated, d.media_errors);
            if let Some(&(was_r, was_m)) = self.counts.get(d.id()) {
                let rose = |now: Option<u64>, was: Option<u64>| matches!((now, was), (Some(n), Some(w)) if n > w);
                if rose(now.0, was_r) || rose(now.1, was_m) {
                    let what = if rose(now.0, was_r) {
                        format!("Reallocated sectors went from {} to {}.", was_r.unwrap_or(0), now.0.unwrap_or(0))
                    } else {
                        format!("Media errors went from {} to {}.", was_m.unwrap_or(0), now.1.unwrap_or(0))
                    };
                    out.push(
                        Report::Event(
                            NewEvent::new(
                                EventCategory::Disk,
                                "smart_degrading",
                                Severity::Warning,
                                name.clone(),
                                format!("{name} is wearing")
                            ).detail(what)
                        )
                    );
                }
            }
            self.counts.insert(d.id().to_string(), now);
        }

        // A drive that's gone was replaced or removed: its problems close.
        let ids: Vec<&str> = status.disks.iter().map(DiskHealth::id).collect();
        for p in open.iter().filter(|p| p.key.starts_with("smart:") && p.key != STALE) {
            let id = p.key.trim_start_matches("smart:").rsplit_once(':').map_or("", |(id, _)| id);
            if !ids.contains(&id) {
                out.push(
                    Report::Resolve(Resolution {
                        key: p.key.clone(),
                        title: format!("{} is no longer reported", p.subject),
                        detail: None,
                    })
                );
            }
        }
        self.counts.retain(|id, _| ids.contains(&id.as_str()));
        out
    }
}

pub fn spawn(events: EventsHandle, mut health: watch::Receiver<Arc<DiskHealthSnapshot>>) {
    tokio::spawn(async move {
        let mut tracker = DriveTracker::default();
        loop {
            let status = health.borrow_and_update().status.clone();
            events.report_all(tracker.observe(&status, &events.open_problems()));
            if health.changed().await.is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(serial: &str) -> DiskHealth {
        DiskHealth {
            device: "/dev/sda".into(),
            model: Some("WDC WD20EZBX".into()),
            serial: Some(serial.into()),
            kind: Some("hdd".into()),
            passed: Some(true),
            temp_c: Some(34.0),
            power_on_hours: Some(10_000),
            reallocated: Some(0),
            pending: Some(0),
            media_errors: None,
            percent_used: None,
        }
    }

    fn status(disks: Vec<DiskHealth>) -> DiskHealthStatus {
        DiskHealthStatus { generated_at: "2026-11-03T00:00:00Z".into(), stale: false, disks }
    }

    fn open(key: &str) -> Problem {
        Problem {
            key: key.into(),
            category: EventCategory::Disk,
            kind: String::new(),
            severity: Severity::Warning,
            subject: "/dev/sda (WDC WD20EZBX)".into(),
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
    fn a_healthy_drive_reports_nothing() {
        assert!(DriveTracker::default().observe(&status(vec![drive("W1")]), &[]).is_empty());
    }

    #[test]
    fn a_failing_drive_and_pending_sectors_open_problems() {
        let mut d = drive("W1");
        d.passed = Some(false);
        d.pending = Some(8);
        let out = DriveTracker::default().observe(&status(vec![d]), &[]);
        assert_eq!(keys(&out), ["open smart:W1:smart_failed", "open smart:W1:smart_pending"]);
        let Report::Open(p) = &out[0] else { unreachable!() };
        assert_eq!(p.title, "/dev/sda (WDC WD20EZBX) failed its health check");
    }

    #[test]
    fn pending_sectors_clearing_resolves() {
        let out = DriveTracker::default().observe(&status(vec![drive("W1")]), &[open("smart:W1:smart_pending")]);
        assert_eq!(keys(&out), ["resolve smart:W1:smart_pending"]);
    }

    #[test]
    fn a_rising_reallocated_count_is_an_event_not_a_problem() {
        let mut t = DriveTracker::default();
        let mut d = drive("W1");
        d.reallocated = Some(4);
        assert!(t.observe(&status(vec![d.clone()]), &[]).is_empty(), "the first count is the baseline");
        d.reallocated = Some(9);
        let out = t.observe(&status(vec![d]), &[]);
        assert_eq!(keys(&out), ["event smart_degrading"]);
        let Report::Event(e) = &out[0] else { unreachable!() };
        assert_eq!(e.detail.as_deref(), Some("Reallocated sectors went from 4 to 9."));
    }

    #[test]
    fn a_worn_nvme_warns() {
        let mut d = drive("N1");
        d.percent_used = Some(93);
        assert_eq!(keys(&DriveTracker::default().observe(&status(vec![d]), &[])), ["open smart:N1:smart_worn"]);
    }

    #[test]
    fn a_stale_file_opens_one_problem_and_leaves_the_drives_alone() {
        let stale = DiskHealthStatus { stale: true, ..status(vec![]) };
        let out = DriveTracker::default().observe(&stale, &[open("smart:W1:smart_failed")]);
        assert_eq!(keys(&out), ["open smart:stale"]);
        let out = DriveTracker::default().observe(&status(vec![drive("W1")]), &[open("smart:stale")]);
        assert_eq!(keys(&out), ["resolve smart:stale"]);
    }

    #[test]
    fn a_replaced_drive_closes_its_problems() {
        let out = DriveTracker::default().observe(&status(vec![drive("NEW")]), &[open("smart:OLD:smart_failed")]);
        assert_eq!(keys(&out), ["resolve smart:OLD:smart_failed"]);
    }
}
