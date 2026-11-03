//! Which services should be running, and which aren't. Pure: the task feeds it
//! the container list and the time, and stores what it says changed.
//!
//! A `helion.service` becomes expected once a container of it has run for
//! [`EXPECT_AFTER`], so a container tried once doesn't count. It is missing
//! once none has run for [`MISSING_AFTER`]: long enough for a deploy or an
//! update to recreate everything, short enough to matter. Two things explain
//! an absence and keep it quiet: a stop or removal through Helion (who did it
//! is shown instead), and an open problem for one of its containers, which
//! already says what happened.

use crate::events::{ human_duration, ProblemSpec, Report, Resolution };
use helion_common::types::{
    ContainerInfo,
    EventCategory,
    ExpectedService,
    ExpectedState,
    Problem,
    Severity,
};
use std::collections::{ BTreeMap, HashMap, HashSet };

pub const EXPECT_AFTER: i64 = 10 * 60;
pub const MISSING_AFTER: i64 = 5 * 60;
/// `last_running` is stored at most this often; in memory it moves every tick.
const PERSIST_EVERY: i64 = 60;

pub const KIND: &str = "missing";

pub fn key(service: &str) -> String {
    format!("service:{service}:missing")
}

/// One expected service as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub service: String,
    pub first_seen: i64,
    pub last_running: i64,
    pub stopped_by: Option<String>,
    pub stopped_at: Option<i64>,
}

#[derive(Debug, Default)]
pub struct Tracker {
    expected: BTreeMap<String, Entry>,
    /// What `last_running` was when each entry was last stored.
    stored_running: HashMap<String, i64>,
    /// Running but not yet expected, with when it was first seen running.
    candidates: HashMap<String, i64>,
    /// Services with containers, running or not, at the last tick.
    present: HashSet<String>,
    running: HashSet<String>,
    /// No absence counts from before this: containers may still be starting
    /// after a reboot that also restarted the agent.
    started: i64,
}

/// What a tick produced: reports for the event log, and entries to store.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub reports: Vec<Report>,
    pub save: Vec<Entry>,
}

impl Tracker {
    pub fn new(entries: Vec<Entry>, now: i64) -> Self {
        Self {
            stored_running: entries
                .iter()
                .map(|e| (e.service.clone(), e.last_running))
                .collect(),
            expected: entries
                .into_iter()
                .map(|e| (e.service.clone(), e))
                .collect(),
            started: now,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, containers: &[ContainerInfo], open: &[Problem], now: i64) -> Output {
        let mut out = Output::default();
        self.present = containers
            .iter()
            .filter_map(|c| c.helion_service.clone())
            .collect();
        self.running = containers
            .iter()
            .filter(|c| c.state == "running")
            .filter_map(|c| c.helion_service.clone())
            .collect();

        for service in &self.running {
            match self.expected.get_mut(service) {
                Some(e) => {
                    e.last_running = now;
                    let marker = e.stopped_at.take().is_some() | e.stopped_by.take().is_some();
                    let stale = now - self.stored_running.get(service).copied().unwrap_or(0) >= PERSIST_EVERY;
                    if marker || stale {
                        self.stored_running.insert(service.clone(), now);
                        out.save.push(e.clone());
                    }
                }
                None => {
                    let since = *self.candidates.entry(service.clone()).or_insert(now);
                    if now - since >= EXPECT_AFTER {
                        let e = Entry {
                            service: service.clone(),
                            first_seen: since,
                            last_running: now,
                            stopped_by: None,
                            stopped_at: None,
                        };
                        self.stored_running.insert(service.clone(), now);
                        self.expected.insert(service.clone(), e.clone());
                        out.save.push(e);
                    }
                }
            }
        }
        let running = &self.running;
        self.candidates.retain(|s, _| running.contains(s) && !self.expected.contains_key(s));

        // Services one of whose containers is already reported down.
        let explained: HashSet<&str> = open
            .iter()
            .filter(|p| p.category == EventCategory::Container && matches!(p.kind.as_str(), "down" | "crash_loop"))
            .filter_map(|p| p.service.as_deref())
            .collect();

        for e in self.expected.values() {
            if self.running.contains(&e.service) || e.stopped_at.is_some() {
                continue;
            }
            let gone_for = now - e.last_running.max(self.started);
            if gone_for < MISSING_AFTER || explained.contains(e.service.as_str()) {
                continue;
            }
            let last = human_duration((now - e.last_running).max(0) as u64);
            let detail = if self.present.contains(&e.service) {
                format!("Its containers are stopped. It was last running {last} ago.")
            } else {
                format!("None of its containers exist. It was last running {last} ago.")
            };
            out.reports.push(
                Report::Open(ProblemSpec {
                    key: key(&e.service),
                    category: EventCategory::Service,
                    kind: KIND,
                    severity: Severity::Warning,
                    subject: e.service.clone(),
                    title: format!("{} is not running", e.service),
                    detail: Some(detail),
                    service: Some(e.service.clone()),
                    depends_on: None,
                })
            );
        }

        for p in open.iter().filter(|p| p.category == EventCategory::Service && p.kind == KIND) {
            let title = match self.expected.get(&p.subject) {
                None => format!("{} is no longer expected", p.subject),
                Some(_) if self.running.contains(&p.subject) => format!("{} is running again", p.subject),
                Some(e) if e.stopped_at.is_some() => {
                    format!("{} was stopped through Helion", p.subject)
                }
                Some(_) => {
                    continue;
                }
            };
            out.reports.push(Report::Resolve(Resolution { key: p.key.clone(), title, detail: None }));
        }
        out
    }

    /// A stop or removal through Helion. Returns the entry to store, if the
    /// service is expected.
    pub fn stopped(&mut self, service: &str, by: &str, now: i64) -> Option<Entry> {
        let e = self.expected.get_mut(service)?;
        e.stopped_by = Some(by.to_string());
        e.stopped_at = Some(now);
        Some(e.clone())
    }

    /// Stops expecting it. It becomes expected again if it runs for
    /// [`EXPECT_AFTER`].
    pub fn forget(&mut self, service: &str) -> bool {
        self.stored_running.remove(service);
        self.expected.remove(service).is_some()
    }

    pub fn list(&self) -> Vec<ExpectedService> {
        self.expected
            .values()
            .map(|e| ExpectedService {
                service: e.service.clone(),
                state: if self.running.contains(&e.service) {
                    ExpectedState::Running
                } else if e.stopped_at.is_some() {
                    ExpectedState::Stopped
                } else {
                    ExpectedState::Missing
                },
                first_seen: e.first_seen,
                last_running: e.last_running,
                present: self.present.contains(&e.service),
                stopped_by: e.stopped_by.clone(),
                stopped_at: e.stopped_at,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(name: &str, service: &str, state: &str) -> ContainerInfo {
        ContainerInfo {
            id: format!("id-{name}"),
            name: name.into(),
            image: "img".into(),
            status: String::new(),
            state: state.into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: None,
            compose_service: None,
            helion_service: Some(service.into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            update_labels: Default::default(),
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
            net_rx_bps: None,
            net_tx_bps: None,
            blk_read_bps: None,
            blk_write_bps: None,
        }
    }

    fn problem(category: EventCategory, kind: &str, subject: &str, service: &str) -> Problem {
        Problem {
            key: if category == EventCategory::Service {
                key(subject)
            } else {
                format!("container:{subject}:{kind}")
            },
            category,
            kind: kind.into(),
            severity: Severity::Warning,
            subject: subject.into(),
            title: String::new(),
            detail: None,
            service: Some(service.into()),
            opened_at: 0,
            event_id: 1,
            depends_on: None,
        }
    }

    fn entry(service: &str, last_running: i64) -> Entry {
        Entry { service: service.into(), first_seen: 0, last_running, stopped_by: None, stopped_at: None }
    }

    fn summary(out: &Output) -> Vec<String> {
        out.reports
            .iter()
            .map(|r| {
                match r {
                    Report::Open(p) => format!("open {}", p.key),
                    Report::Resolve(r) => format!("resolve {}: {}", r.key, r.title),
                    Report::Event(e) => format!("event {}", e.kind),
                }
            })
            .collect()
    }

    #[test]
    fn a_service_is_expected_only_after_running_a_while() {
        let mut t = Tracker::new(vec![], 0);
        let web = [container("web", "web", "running")];
        assert!(t.observe(&web, &[], 0).save.is_empty());
        assert!(t.observe(&web, &[], EXPECT_AFTER - 1).save.is_empty());
        let out = t.observe(&web, &[], EXPECT_AFTER);
        assert_eq!(out.save, [Entry { first_seen: 0, ..entry("web", EXPECT_AFTER) }]);

        // One tried and stopped never becomes expected.
        let mut t = Tracker::new(vec![], 0);
        t.observe(&[container("try", "try", "running")], &[], 0);
        t.observe(&[container("try", "try", "exited")], &[], 60);
        assert!(t.observe(&[container("try", "try", "running")], &[], EXPECT_AFTER).save.is_empty());
    }

    #[test]
    fn an_absence_opens_after_the_grace_and_says_whether_containers_exist() {
        let mut t = Tracker::new(vec![entry("web", 1_000)], 1_000);
        assert!(summary(&t.observe(&[], &[], 1_000 + MISSING_AFTER - 1)).is_empty());

        let out = t.observe(&[container("web", "web", "exited")], &[], 1_000 + MISSING_AFTER);
        assert_eq!(summary(&out), ["open service:web:missing"]);
        let Report::Open(p) = &out.reports[0] else { unreachable!() };
        assert_eq!(p.title, "web is not running");
        assert_eq!(p.detail.as_deref(), Some("Its containers are stopped. It was last running 5m ago."));

        let out = t.observe(&[], &[], 1_000 + MISSING_AFTER);
        let Report::Open(p) = &out.reports[0] else { unreachable!() };
        assert!(p.detail.as_deref().unwrap().starts_with("None of its containers exist."));
    }

    #[test]
    fn a_restart_of_the_agent_waits_before_reporting() {
        // Last ran long ago (the host was off), the agent just started.
        let mut t = Tracker::new(vec![entry("web", 0)], 100_000);
        assert!(t.observe(&[], &[], 100_000 + 60).reports.is_empty());
        assert_eq!(summary(&t.observe(&[], &[], 100_000 + MISSING_AFTER)), ["open service:web:missing"]);
    }

    #[test]
    fn running_again_resolves() {
        let mut t = Tracker::new(vec![entry("web", 0)], 0);
        let open = [problem(EventCategory::Service, KIND, "web", "web")];
        let out = t.observe(&[container("web", "web", "running")], &open, 1_000);
        assert_eq!(summary(&out), ["resolve service:web:missing: web is running again"]);
    }

    #[test]
    fn a_stop_through_helion_is_not_a_problem() {
        let mut t = Tracker::new(vec![entry("web", 0)], 0);
        assert!(t.stopped("web", "Pat", 10).is_some());
        assert!(t.stopped("nope", "Pat", 10).is_none(), "only expected services");
        assert!(t.observe(&[container("web", "web", "exited")], &[], 10_000).reports.is_empty());
        assert_eq!(t.list()[0].state, ExpectedState::Stopped);
        assert_eq!(t.list()[0].stopped_by.as_deref(), Some("Pat"));

        // Starting it again clears the marker, and that is stored.
        let out = t.observe(&[container("web", "web", "running")], &[], 10_010);
        assert_eq!(out.save[0].stopped_by, None);
        assert_eq!(t.list()[0].state, ExpectedState::Running);
    }

    #[test]
    fn a_container_problem_already_explains_the_absence() {
        let mut t = Tracker::new(vec![entry("web", 0)], 0);
        let open = [problem(EventCategory::Container, "down", "web-1", "web")];
        assert!(t.observe(&[container("web-1", "web", "exited")], &open, 10_000).reports.is_empty());
    }

    #[test]
    fn forgetting_resolves_and_it_is_learned_again_by_running() {
        let mut t = Tracker::new(vec![entry("web", 0)], 0);
        assert!(t.forget("web"));
        let open = [problem(EventCategory::Service, KIND, "web", "web")];
        let out = t.observe(&[], &open, 10_000);
        assert_eq!(summary(&out), ["resolve service:web:missing: web is no longer expected"]);
        assert!(t.list().is_empty());
    }

    #[test]
    fn last_running_is_stored_once_a_minute_not_every_tick() {
        let mut t = Tracker::new(vec![entry("web", 0)], 0);
        let web = [container("web", "web", "running")];
        assert_eq!(t.observe(&web, &[], 100).save.len(), 1);
        assert!(t.observe(&web, &[], 115).save.is_empty());
        assert_eq!(t.observe(&web, &[], 160).save.len(), 1);
    }
}
