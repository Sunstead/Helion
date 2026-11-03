//! Acknowledgements and silences: what a person asked to stop hearing about.
//!
//! - An **acknowledged** problem sends nothing more: no late or repeated
//!   opening. Its recovery still goes out, so the person who took it hears
//!   that it cleared.
//! - A **silence** covers a node or a service for a while. Openings in it are
//!   held; when it ends, those still open are sent once, so planned work that
//!   left something broken still pages. Recoveries of held openings and
//!   one-off events in it are dropped: nobody was told, so there's nothing to
//!   take back.
//!
//! Only delivery changes. The event log records everything as it happened.

use helion_common::types::{ Event, Problem, ProblemState, Silence };
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Mutes {
    /// Openings held by a silence, by problem key.
    held: HashMap<String, Event>,
}

fn silenced(e: &Event, silences: &[Silence], now: i64) -> bool {
    silences.iter().any(|s| s.covers(e.service.as_deref(), now))
}

impl Mutes {
    /// Whether `e` may go on to the channels.
    pub fn check(&mut self, e: &Event, open: &[Problem], silences: &[Silence], now: i64) -> bool {
        match &e.problem {
            None => !silenced(e, silences, now),
            Some(p) if p.state == ProblemState::Opened => {
                if open.iter().any(|o| o.key == p.key && o.acked_by.is_some()) {
                    return false;
                }
                if silenced(e, silences, now) {
                    self.held.insert(p.key.clone(), e.clone());
                    return false;
                }
                true
            }
            Some(p) => self.held.remove(&p.key).is_none() && !silenced(e, silences, now),
        }
    }

    /// Held openings whose silence has ended and that are still open and
    /// unacknowledged. The rest wait, or are dropped once there's nothing to say.
    pub fn release(&mut self, open: &[Problem], silences: &[Silence], now: i64) -> Vec<Event> {
        let mut out = Vec::new();
        self.held.retain(|key, e| {
            let Some(o) = open.iter().find(|o| &o.key == key) else {
                return false;
            };
            if o.acked_by.is_some() {
                return false;
            }
            if silenced(e, silences, now) {
                return true;
            }
            out.push(e.clone());
            false
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helion_common::types::{ EventCategory, ProblemRef, Severity, SilenceScope };

    fn event(key: Option<(&str, ProblemState)>, service: &str) -> Event {
        Event {
            id: 1,
            at: 0,
            category: EventCategory::Container,
            kind: "down".into(),
            severity: Severity::Error,
            subject: "web-1".into(),
            title: "web-1 is down".into(),
            detail: None,
            actor: None,
            service: Some(service.into()),
            problem: key.map(|(k, state)| ProblemRef { key: k.into(), state }),
        }
    }

    fn open(key: &str, acked: bool) -> Problem {
        Problem {
            key: key.into(),
            category: EventCategory::Container,
            kind: "down".into(),
            severity: Severity::Error,
            subject: "web-1".into(),
            title: String::new(),
            detail: None,
            service: Some("web".into()),
            opened_at: 0,
            event_id: 1,
            depends_on: None,
            acked_by: acked.then(|| "Pat".into()),
            acked_at: acked.then_some(5),
        }
    }

    fn silence(scope: SilenceScope, target: Option<&str>, until: Option<i64>) -> Silence {
        Silence {
            id: 1,
            scope,
            target: target.map(Into::into),
            until,
            reason: None,
            created_by: "Pat".into(),
            created_at: 0,
        }
    }

    const OPENED: ProblemState = ProblemState::Opened;
    const RESOLVED: ProblemState = ProblemState::Resolved;

    #[test]
    fn an_acknowledged_problem_stays_quiet_but_its_recovery_goes_out() {
        let mut m = Mutes::default();
        let acked = [open("k", true)];
        assert!(!m.check(&event(Some(("k", OPENED)), "web"), &acked, &[], 10), "a late opening");
        assert!(m.check(&event(Some(("k", RESOLVED)), "web"), &[], &[], 20));
    }

    #[test]
    fn a_service_silence_holds_only_that_service() {
        let mut m = Mutes::default();
        let s = [silence(SilenceScope::Service, Some("web"), Some(100))];
        assert!(!m.check(&event(Some(("k", OPENED)), "web"), &[open("k", false)], &s, 10));
        assert!(m.check(&event(Some(("j", OPENED)), "db"), &[], &s, 10));
        assert!(!m.check(&event(None, "web"), &[], &s, 10), "a one-off event in it");
    }

    #[test]
    fn a_held_opening_is_sent_once_when_the_silence_ends_if_still_open() {
        let mut m = Mutes::default();
        let s = [silence(SilenceScope::Node, None, Some(100))];
        let still = [open("k", false)];
        m.check(&event(Some(("k", OPENED)), "web"), &still, &s, 10);
        assert!(m.release(&still, &s, 50).is_empty(), "still silenced");
        assert_eq!(m.release(&still, &s, 100).len(), 1, "ended at 100");
        assert!(m.release(&still, &s, 160).is_empty(), "only once");
    }

    #[test]
    fn a_held_opening_that_cleared_says_nothing_either_way() {
        let mut m = Mutes::default();
        let s = [silence(SilenceScope::Node, None, None)];
        m.check(&event(Some(("k", OPENED)), "web"), &[open("k", false)], &s, 10);
        assert!(!m.check(&event(Some(("k", RESOLVED)), "web"), &[], &[], 20), "its recovery");
        assert!(m.release(&[], &[], 30).is_empty());
    }

    #[test]
    fn acknowledging_a_held_opening_drops_it() {
        let mut m = Mutes::default();
        let s = [silence(SilenceScope::Node, None, Some(100))];
        m.check(&event(Some(("k", OPENED)), "web"), &[open("k", false)], &s, 10);
        assert!(m.release(&[open("k", true)], &[], 200).is_empty());
    }
}
