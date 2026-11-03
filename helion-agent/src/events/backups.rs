//! Backups, from the status file the backup job writes (see `backups.rs`):
//! failed, stale and interrupted runs are problems; a finished run is an
//! event.

use super::{ human_bytes, human_duration, EventsHandle, NewEvent, ProblemSpec, Report, Resolution };
use crate::{ backups::{ parse_rfc3339, BackupSnapshot }, sample::host::unix_now };
use helion_common::types::{
    BackupsStatus,
    CheckResult,
    EventCategory,
    Problem,
    RestoreTestStatus,
    Severity,
    StepStatus,
};
use std::sync::Arc;
use tokio::sync::watch;

/// The timer fired and nothing reported since: a backup is either running or
/// died. Only after this long is it the second. A normal run takes minutes;
/// one that re-reads every file has taken hours.
const INTERRUPTED_AFTER: i64 = 6 * 3_600;

const FAILED: &str = "backup:failed";
const STALE: &str = "backup:stale";
const INTERRUPTED: &str = "backup:interrupted";
const RESTORE_TEST: &str = "backup:restore_test";
const RESTORE_TEST_STALE: &str = "backup:restore_test_stale";
/// A weekly test, plus a week's grace.
const RESTORE_TEST_EVERY: i64 = 15 * 86_400;

#[derive(Default)]
pub struct BackupTracker {
    /// `generated_at` of the last run seen, to notice a new one.
    seen: Option<String>,
    /// `finished_at` of the last restore test seen.
    seen_test: Option<String>,
}

fn spec(key: &str, kind: &'static str, s: &BackupsStatus, title: String, detail: String) -> ProblemSpec {
    ProblemSpec {
        key: key.into(),
        category: EventCategory::Backup,
        kind,
        severity: Severity::Error,
        subject: s.repo_label.clone(),
        title,
        detail: Some(detail),
        service: None,
        depends_on: None,
    }
}

fn is_open(open: &[Problem], key: &str) -> bool {
    open.iter().any(|p| p.key == key)
}

fn resolve(key: &str, title: &str, detail: Option<String>) -> Report {
    Report::Resolve(Resolution { key: key.into(), title: title.into(), detail })
}

/// "Free space: 12G free, need 50G", one per failed step.
fn failed_steps(s: &BackupsStatus) -> Vec<String> {
    let steps: [(&str, &Option<StepStatus>); 4] = [
        ("Database dumps", &s.postgres_dump),
        ("Copy to second disk", &s.state_copy),
        ("Free space", &s.space_check),
        ("Heartbeat", &s.heartbeat),
    ];
    steps
        .iter()
        .filter_map(|(label, step)| {
            let step = step.as_ref().filter(|st| !st.ok)?;
            Some(match &step.message {
                Some(m) => format!("{label}: {m}"),
                None => format!("{label} failed"),
            })
        })
        .collect()
}

impl BackupTracker {
    pub fn observe(&mut self, s: &BackupsStatus, open: &[Problem], now: i64) -> Vec<Report> {
        let mut out = Vec::new();
        let new_run = !s.generated_at.is_empty() && self.seen.as_deref() != Some(s.generated_at.as_str());
        // The first status seen after the agent starts is not a new run.
        let announce = new_run && self.seen.is_some();
        if new_run {
            self.seen = Some(s.generated_at.clone());
        }
        let took = s.duration_secs.map(human_duration);

        match s.last_exit_code {
            Some(code) if code != 0 => {
                let steps = failed_steps(s);
                let detail = if steps.is_empty() {
                    format!("The backup job exited with code {code}.")
                } else {
                    steps.join(". ") + "."
                };
                out.push(Report::Open(spec(FAILED, "failed", s, format!("Backup failed (exit {code})"), detail)));
            }
            Some(_) => {
                if is_open(open, FAILED) {
                    out.push(resolve(FAILED, "Backups are working again", took.as_ref().map(|t| format!("The last one took {t}."))));
                } else if announce {
                    let mut detail = format!("{} snapshots", s.snapshot_count);
                    if let Some(size) = s.repo_size_bytes {
                        detail += &format!(", repository {}", human_bytes(size));
                    }
                    let title = match &took {
                        Some(t) => format!("Backup finished in {t}"),
                        None => "Backup finished".into(),
                    };
                    out.push(
                        Report::Event(NewEvent::new(EventCategory::Backup, "completed", Severity::Info, &s.repo_label, title).detail(detail + "."))
                    );
                }
            }
            None => {}
        }

        if s.stale {
            let (title, detail) = match parse_rfc3339(&s.generated_at) {
                Some(at) => ("Backups have stopped".to_string(), format!("The last backup reported {} ago.", human_duration((now - at).max(0) as u64))),
                None => ("No backup status".to_string(), "The backup status file is missing or unreadable.".to_string()),
            };
            out.push(Report::Open(spec(STALE, "stale", s, title, detail)));
        } else if is_open(open, STALE) {
            out.push(resolve(STALE, "Backups are reporting again", None));
        }

        let fired = s.timer_last_fired.as_deref().and_then(parse_rfc3339);
        let wrote = parse_rfc3339(&s.generated_at);
        match (fired, wrote) {
            // Running, or died: only after this long is it the second.
            (Some(f), Some(w)) if f > w && now - f >= INTERRUPTED_AFTER => {
                out.push(
                    Report::Open(
                        spec(
                            INTERRUPTED,
                            "interrupted",
                            s,
                            "The last backup didn't finish".into(),
                            format!("The timer fired {} ago, but the job never reported.", human_duration((now - f) as u64))
                        )
                    )
                );
            }
            (Some(f), Some(w)) if f > w => {}
            _ if is_open(open, INTERRUPTED) => out.push(resolve(INTERRUPTED, "A backup finished again", None)),
            _ => {}
        }
        if let Some(t) = &s.restore_test {
            out.extend(self.observe_test(t, s, open, now));
        }
        out
    }

    fn observe_test(&mut self, t: &RestoreTestStatus, s: &BackupsStatus, open: &[Problem], now: i64) -> Vec<Report> {
        let mut out = Vec::new();
        let new_test = self.seen_test.as_deref() != Some(t.finished_at.as_str());
        let announce = new_test && self.seen_test.is_some();
        if new_test {
            self.seen_test = Some(t.finished_at.clone());
        }

        let died = t.exit_code.is_some_and(|c| c != 0) && t.failed == 0;
        if t.failed > 0 || died {
            let fails: Vec<&str> = t.checks
                .iter()
                .filter(|c| c.result == CheckResult::Fail)
                .map(|c| c.message.as_str())
                .take(3)
                .collect();
            let detail = if fails.is_empty() {
                format!("It stopped before finishing (exit {}).", t.exit_code.unwrap_or(1))
            } else {
                fails.join(". ") + "."
            };
            let title = if died { "The restore test didn't finish".to_string() } else { format!("The restore test failed {} check{}", t.failed, if t.failed == 1 { "" } else { "s" }) };
            out.push(Report::Open(spec(RESTORE_TEST, "restore_test_failed", s, title, detail)));
        } else if is_open(open, RESTORE_TEST) {
            out.push(resolve(RESTORE_TEST, "The restore test passes again", Some(format!("{} checks passed.", t.passed))));
        } else if announce {
            let mut detail = format!("{} checks passed", t.passed);
            if t.warned > 0 {
                detail += &format!(", {} with a warning", t.warned);
            }
            out.push(
                Report::Event(
                    NewEvent::new(EventCategory::Backup, "restore_test_passed", Severity::Info, &s.repo_label, "Restore test passed").detail(detail + ".")
                )
            );
        }

        match parse_rfc3339(&t.finished_at) {
            Some(at) if now - at > RESTORE_TEST_EVERY => {
                out.push(
                    Report::Open(ProblemSpec {
                        severity: Severity::Warning,
                        ..spec(
                            RESTORE_TEST_STALE,
                            "restore_test_stale",
                            s,
                            "The restore test hasn't run lately".into(),
                            format!("The last one finished {} ago; it should run weekly. Check its timer on the host.", human_duration((now - at) as u64))
                        )
                    })
                );
            }
            _ if is_open(open, RESTORE_TEST_STALE) => out.push(resolve(RESTORE_TEST_STALE, "The restore test ran again", None)),
            _ => {}
        }
        out
    }
}

pub fn spawn(events: EventsHandle, mut backups: watch::Receiver<Arc<BackupSnapshot>>) {
    tokio::spawn(async move {
        let mut tracker = BackupTracker::default();
        loop {
            let status = backups.borrow_and_update().status.clone();
            events.report_all(tracker.observe(&status, &events.open_problems(), unix_now()));
            if backups.changed().await.is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::format_rfc3339;

    const NOW: i64 = 1_790_000_000;

    fn status(exit: Option<i32>, generated_at: i64) -> BackupsStatus {
        BackupsStatus {
            generated_at: format_rfc3339(generated_at),
            stale: false,
            last_run: None,
            next_run: None,
            timer_last_fired: Some(format_rfc3339(generated_at - 900)),
            last_exit_code: exit,
            duration_secs: Some(840),
            repo_label: "jupiter/restic".into(),
            repo_size_bytes: Some(184_000_000_000),
            snapshot_count: 12,
            snapshots: vec![],
            retention: None,
            postgres_dump: Some(StepStatus { ok: true, at: None, message: None }),
            state_copy: Some(StepStatus { ok: true, at: None, message: None }),
            space_check: Some(StepStatus { ok: true, at: None, message: Some("187G free".into()) }),
            heartbeat: None,
            requests: vec![],
            restore_test: None,
            databases: vec![],
        }
    }

    fn test_result(failed: u32, finished_at: i64) -> RestoreTestStatus {
        use helion_common::types::RestoreCheck;
        let mut checks = vec![RestoreCheck { result: CheckResult::Pass, message: "primary: gitea_db restored (48 tables)".into() }];
        if failed > 0 {
            checks.push(RestoreCheck { result: CheckResult::Fail, message: "state: immich.sql.gz is missing from the snapshot".into() });
        }
        RestoreTestStatus {
            finished_at: format_rfc3339(finished_at),
            duration_secs: Some(300),
            exit_code: Some(if failed > 0 { 1 } else { 0 }),
            passed: 11,
            warned: 0,
            failed,
            checks,
        }
    }

    #[test]
    fn restore_tests_open_resolve_and_go_stale() {
        let mut t = BackupTracker::default();
        let mut s = status(Some(0), NOW);
        t.observe(&s, &[], NOW);

        s.restore_test = Some(test_result(0, NOW - 60));
        assert!(t.observe(&s, &[], NOW).is_empty(), "the first test seen after start is old news");

        s.restore_test = Some(test_result(1, NOW));
        let out = t.observe(&s, &[], NOW);
        let [Report::Open(p)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(p.key, RESTORE_TEST);
        assert_eq!(p.title, "The restore test failed 1 check");
        assert_eq!(p.detail.as_deref(), Some("state: immich.sql.gz is missing from the snapshot."));

        s.restore_test = Some(test_result(0, NOW + 600));
        let out = t.observe(&s, &[open(RESTORE_TEST)], NOW + 600);
        assert!(matches!(out.as_slice(), [Report::Resolve(r)] if r.key == RESTORE_TEST), "{out:?}");

        s.restore_test = Some(test_result(0, NOW + 1_200));
        let out = t.observe(&s, &[], NOW + 1_200);
        assert!(matches!(out.as_slice(), [Report::Event(e)] if e.title == "Restore test passed"), "{out:?}");

        let out = t.observe(&s, &[], NOW + 20 * 86_400);
        let [Report::Open(p)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!((p.key.as_str(), p.severity), (RESTORE_TEST_STALE, Severity::Warning));
    }

    fn open(key: &str) -> Problem {
        Problem {
            key: key.into(),
            category: EventCategory::Backup,
            kind: "x".into(),
            severity: Severity::Error,
            subject: "jupiter/restic".into(),
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

    #[test]
    fn a_failed_run_names_the_failed_steps() {
        let mut s = status(Some(1), NOW);
        s.space_check = Some(StepStatus { ok: false, at: None, message: Some("12G free, need 50G".into()) });
        let out = BackupTracker::default().observe(&s, &[], NOW);
        let [Report::Open(p)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(p.key, FAILED);
        assert_eq!(p.title, "Backup failed (exit 1)");
        assert_eq!(p.detail.as_deref(), Some("Free space: 12G free, need 50G."));
    }

    #[test]
    fn a_good_run_resolves_a_failure_or_is_announced() {
        let mut t = BackupTracker::default();
        assert!(t.observe(&status(Some(0), NOW - 86_400), &[], NOW).is_empty(), "the first status after start is old news");

        let out = t.observe(&status(Some(0), NOW), &[], NOW);
        let [Report::Event(e)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(e.title, "Backup finished in 14m");
        assert_eq!(e.detail.as_deref(), Some("12 snapshots, repository 184 GB."));
        assert!(t.observe(&status(Some(0), NOW), &[], NOW + 30).is_empty(), "the same run again");

        let out = t.observe(&status(Some(0), NOW + 86_400), &[open(FAILED)], NOW + 86_400);
        let [Report::Resolve(r)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(r.key, FAILED);
    }

    #[test]
    fn staleness_opens_and_clears() {
        let mut s = status(Some(0), NOW - 2 * 86_400);
        s.stale = true;
        let out = BackupTracker::default().observe(&s, &[], NOW);
        let [Report::Open(p)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(p.title, "Backups have stopped");
        assert_eq!(p.detail.as_deref(), Some("The last backup reported 2d 0h ago."));

        let out = BackupTracker::default().observe(&status(Some(0), NOW), &[open(STALE)], NOW);
        assert!(matches!(out.as_slice(), [Report::Resolve(r)] if r.key == STALE), "{out:?}");
    }

    /// The timer fires at 03:00 and the status is written when the job ends.
    /// In between, the backup is running, not interrupted.
    #[test]
    fn a_running_backup_is_not_interrupted() {
        let mut s = status(Some(0), NOW - 86_400);
        s.timer_last_fired = Some(format_rfc3339(NOW - 3_600));
        assert!(BackupTracker::default().observe(&s, &[], NOW).is_empty());

        s.timer_last_fired = Some(format_rfc3339(NOW - INTERRUPTED_AFTER));
        let out = BackupTracker::default().observe(&s, &[], NOW);
        assert!(matches!(out.as_slice(), [Report::Open(p)] if p.key == INTERRUPTED), "{out:?}");

        // It reported after all.
        let out = BackupTracker::default().observe(&status(Some(0), NOW), &[open(INTERRUPTED)], NOW);
        assert!(matches!(out.as_slice(), [Report::Resolve(r)] if r.key == INTERRUPTED), "{out:?}");
    }
}
