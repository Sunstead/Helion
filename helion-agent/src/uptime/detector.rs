//! Turns check results into problems: a check is down after three failures
//! in a row and back up after two passes, so one dropped request never pages
//! anyone. Certificates get a problem when renewal looks overdue.
//!
//! Pure: callers pass the open problems and the time.

use super::cert::CertValidity;
use crate::events::{ human_duration, ProblemSpec, Report, Resolution };
use helion_common::types::{ CheckState, EventCategory, Problem, Severity };
use std::collections::{ BTreeMap, HashMap };

pub const DOWN_AFTER: u32 = 3;
pub const UP_AFTER: u32 = 2;
/// Under this long left is an error whatever the certificate's lifetime.
const CRITICAL_SECS: i64 = 3 * 86_400;
const CERT_PREFIX: &str = "uptime:cert:";

/// Passes or failures in a row.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Streak {
    pub oks: u32,
    pub fails: u32,
}

/// What a check reports as.
pub struct Subject<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub target: &'a str,
    pub service: Option<&'a str>,
    /// The peer the target is reached through, if any.
    pub via_peer: Option<&'a str>,
}

pub fn down_key(id: &str) -> String {
    format!("uptime:{id}:down")
}

fn find<'a>(open: &'a [Problem], key: &str) -> Option<&'a Problem> {
    open.iter().find(|p| p.key == key)
}

/// Records one result. Returns the check's state, and what to report.
pub fn observe(
    streak: &mut Streak,
    s: &Subject,
    ok: bool,
    detail: Option<&str>,
    open: &[Problem],
    now: i64
) -> (CheckState, Option<Report>) {
    let key = down_key(s.id);
    let problem = find(open, &key);
    if ok {
        streak.oks += 1;
        streak.fails = 0;
        return match problem {
            Some(p) if streak.oks >= UP_AFTER => {
                let resolution = Resolution {
                    key,
                    title: format!("{} is back up", s.name),
                    detail: Some(format!("It was down for {}.", human_duration((now - p.opened_at).max(0) as u64))),
                };
                (CheckState::Up, Some(Report::Resolve(resolution)))
            }
            Some(_) => (CheckState::Down, None),
            None => (CheckState::Up, None),
        };
    }

    streak.fails += 1;
    streak.oks = 0;
    if streak.fails >= DOWN_AFTER {
        // Repeating an open is free; the event log keeps the first.
        let spec = ProblemSpec {
            key,
            category: EventCategory::Uptime,
            kind: "down",
            severity: Severity::Error,
            subject: s.name.to_string(),
            title: format!("{} is down", s.name),
            detail: Some(match detail {
                Some(d) => format!("{d} from {}", s.target),
                None => format!("No answer from {}", s.target),
            }),
            service: s.service.map(str::to_string),
            depends_on: s.via_peer.map(str::to_string),
        };
        return (CheckState::Down, Some(Report::Open(spec)));
    }
    let state = if problem.is_some() { CheckState::Down } else { CheckState::Failing };
    (state, None)
}

/// A check that was removed or turned off, or whose service is gone.
pub fn gone(id: &str, name: &str, open: &[Problem]) -> Option<Report> {
    let key = down_key(id);
    find(open, &key)?;
    Some(Report::Resolve(Resolution { key, title: format!("{name} is no longer checked"), detail: None }))
}

fn cert_problem(serial: &str, host: &str, v: &CertValidity, now: i64) -> Option<ProblemSpec> {
    let left = v.not_after - now;
    let lifetime = (v.not_after - v.not_before).max(1);
    let (kind, severity, key) = if left < CRITICAL_SECS {
        ("cert_critical", Severity::Error, format!("{CERT_PREFIX}{serial}:critical"))
    } else if left * 4 < lifetime {
        ("cert_expiring", Severity::Warning, format!("{CERT_PREFIX}{serial}"))
    } else {
        return None;
    };
    let title = if left <= 0 {
        format!("The certificate for {host} has expired")
    } else {
        format!("The certificate for {host} expires in {}", human_duration(left as u64))
    };
    Some(ProblemSpec {
        key,
        category: EventCategory::Uptime,
        kind,
        severity,
        subject: host.to_string(),
        title,
        detail: Some(
            "It should have been renewed by now. Check the reverse proxy's logs: renewal may be failing.".into()
        ),
        service: None,
        depends_on: None,
    })
}

/// Certificates close to expiry, from the one each HTTPS host last served.
/// A certificate's problems resolve once its host serves a different one.
pub fn certs(presented: &HashMap<String, CertValidity>, open: &[Problem], now: i64) -> Vec<Report> {
    // One problem per certificate, named after its first host, however many
    // hosts share it (a wildcard).
    let mut by_serial: BTreeMap<&str, (&str, &CertValidity)> = BTreeMap::new();
    for (host, v) in presented {
        let entry = by_serial.entry(&v.serial).or_insert((host, v));
        if host.as_str() < entry.0 {
            entry.0 = host;
        }
    }
    let mut reports: Vec<Report> = by_serial
        .iter()
        .filter_map(|(serial, (host, v))| cert_problem(serial, host, v, now))
        .map(Report::Open)
        .collect();

    for p in open.iter().filter(|p| p.key.starts_with(CERT_PREFIX)) {
        let serial = p.key[CERT_PREFIX.len()..].trim_end_matches(":critical");
        if presented.get(&p.subject).is_some_and(|v| v.serial != serial) {
            reports.push(
                Report::Resolve(Resolution {
                    key: p.key.clone(),
                    title: format!("The certificate for {} was renewed", p.subject),
                    detail: None,
                })
            );
        }
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBJECT: Subject = Subject {
        id: "svc:immich",
        name: "immich",
        target: "https://immich.example.net/",
        service: Some("immich"),
        via_peer: None,
    };

    fn problem(key: &str, subject: &str, opened_at: i64) -> Problem {
        Problem {
            key: key.into(),
            category: EventCategory::Uptime,
            kind: "down".into(),
            severity: Severity::Error,
            subject: subject.into(),
            title: String::new(),
            detail: None,
            service: None,
            opened_at,
            event_id: 1,
            depends_on: None,
            acked_by: None,
            acked_at: None,
        }
    }

    #[test]
    fn three_failures_in_a_row_open_a_problem() {
        let mut streak = Streak::default();
        let fail = |streak: &mut Streak| observe(streak, &SUBJECT, false, Some("HTTP 502"), &[], 0);
        assert_eq!(fail(&mut streak), (CheckState::Failing, None));
        assert_eq!(fail(&mut streak), (CheckState::Failing, None));
        let (state, report) = fail(&mut streak);
        assert_eq!(state, CheckState::Down);
        let Some(Report::Open(spec)) = report else { panic!("expected an open") };
        assert_eq!(spec.title, "immich is down");
        assert_eq!(spec.detail.as_deref(), Some("HTTP 502 from https://immich.example.net/"));
        assert_eq!(spec.key, "uptime:svc:immich:down");
    }

    #[test]
    fn a_pass_in_between_starts_again() {
        let mut streak = Streak::default();
        observe(&mut streak, &SUBJECT, false, None, &[], 0);
        observe(&mut streak, &SUBJECT, false, None, &[], 0);
        assert_eq!(observe(&mut streak, &SUBJECT, true, None, &[], 0), (CheckState::Up, None));
        assert_eq!(observe(&mut streak, &SUBJECT, false, None, &[], 0).1, None);
    }

    #[test]
    fn two_passes_resolve_with_how_long_it_was_down() {
        let open = [problem("uptime:svc:immich:down", "immich", 1_000)];
        let mut streak = Streak::default();
        assert_eq!(observe(&mut streak, &SUBJECT, true, None, &open, 1_200), (CheckState::Down, None));
        let (state, report) = observe(&mut streak, &SUBJECT, true, None, &open, 1_260);
        assert_eq!(state, CheckState::Up);
        let Some(Report::Resolve(r)) = report else { panic!("expected a resolve") };
        assert_eq!(r.title, "immich is back up");
        assert_eq!(r.detail.as_deref(), Some("It was down for 4m."));
    }

    #[test]
    fn a_check_that_goes_away_resolves_its_problem() {
        let open = [problem("uptime:svc:immich:down", "immich", 0)];
        assert!(matches!(gone("svc:immich", "immich", &open), Some(Report::Resolve(_))));
        assert_eq!(gone("svc:immich", "immich", &[]), None);
    }

    fn validity(serial: &str, not_before: i64, not_after: i64) -> CertValidity {
        CertValidity { serial: serial.into(), not_before, not_after }
    }

    #[test]
    fn certificates_warn_in_their_last_quarter_and_fail_in_the_last_days() {
        const DAY: i64 = 86_400;
        let presented = HashMap::from([
            ("b.example.net".to_string(), validity("aa", 0, 90 * DAY)),
            ("a.example.net".to_string(), validity("aa", 0, 90 * DAY)),
        ]);
        assert!(certs(&presented, &[], 60 * DAY).is_empty(), "renewal isn't due yet");

        let reports = certs(&presented, &[], 70 * DAY);
        assert_eq!(reports.len(), 1, "one problem for the shared certificate");
        let Report::Open(spec) = &reports[0] else { panic!() };
        assert_eq!(spec.severity, Severity::Warning);
        assert_eq!(spec.title, "The certificate for a.example.net expires in 20d 0h");

        let reports = certs(&presented, &[], 88 * DAY);
        let Report::Open(spec) = &reports[0] else { panic!() };
        assert_eq!(spec.severity, Severity::Error);
        assert_eq!(spec.key, "uptime:cert:aa:critical");
    }

    #[test]
    fn a_renewed_certificate_resolves_the_old_ones_problems() {
        let open = [
            problem("uptime:cert:aa", "a.example.net", 0),
            problem("uptime:cert:aa:critical", "a.example.net", 0),
        ];
        let same = HashMap::from([("a.example.net".to_string(), validity("aa", 0, 100))]);
        assert!(certs(&same, &open, 1_000).iter().all(|r| matches!(r, Report::Open(_))));

        let renewed = HashMap::from([("a.example.net".to_string(), validity("bb", 0, 1_000_000))]);
        let reports = certs(&renewed, &open, 1_000);
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| matches!(r, Report::Resolve(_))));
    }
}
