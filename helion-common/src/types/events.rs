use serde::{ Deserialize, Serialize };
use ts_rs::TS;

/// How much an event matters. Ordered, so a notification rule can compare
/// against a minimum.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    Container,
    Backup,
    Disk,
    /// Something an admin did through Helion.
    Action,
    Wol,
    /// The agent and the host it runs on: restarts, upgrades, reboots.
    Agent,
    /// HTTP and TCP checks, and certificate expiry. Absent before 0.6.
    Uptime,
    /// Image updates: applied, failed, broken. Absent before 0.8.
    Update,
    /// Other agents this one exchanges heartbeats with. Absent before 0.11.
    Peer,
    /// A service that is expected to run and isn't. Absent before 0.15.
    Service,
    /// The host's own load: CPU, memory and swap pressure. Absent before 0.15.
    Host,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum ProblemState {
    Opened,
    Resolved,
}

/// The same names serde uses, for storing these as text.
macro_rules! text_enum {
    ($t:ty { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $t {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($name => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

text_enum!(Severity { Info => "info", Warning => "warning", Error => "error" });
text_enum!(EventCategory {
    Container => "container",
    Backup => "backup",
    Disk => "disk",
    Action => "action",
    Wol => "wol",
    Agent => "agent",
    Uptime => "uptime",
    Update => "update",
    Peer => "peer",
    Service => "service",
    Host => "host",
});
text_enum!(ProblemState { Opened => "opened", Resolved => "resolved" });

/// Ties an event to the problem it opened or resolved.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct ProblemRef {
    pub key: String,
    pub state: ProblemState,
}

/// One row of a node's event log. Append-only: nothing edits an event after
/// it's written.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct Event {
    /// Increasing per node; the cursor for `?after=` and `?before=`.
    #[ts(type = "number")]
    pub id: i64,
    /// Unix seconds.
    #[ts(type = "number")]
    pub at: i64,
    pub category: EventCategory,
    /// What happened within the category, e.g. `crashed`, `unhealthy`.
    pub kind: String,
    /// For a resolution, the severity of the problem it resolved.
    pub severity: Severity,
    /// What it happened to: a container name, a mount, a machine.
    pub subject: String,
    pub title: String,
    pub detail: Option<String>,
    /// Who did it, for actions taken through Helion.
    pub actor: Option<String>,
    /// The subject's `helion.service` label, when it has one.
    pub service: Option<String>,
    pub problem: Option<ProblemRef>,
}

/// A condition that's true now, such as a container that's unhealthy. It was
/// opened by an event and is closed by a later one.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct Problem {
    /// Stable for the condition, e.g. `container:immich-server:unhealthy`.
    pub key: String,
    pub category: EventCategory,
    pub kind: String,
    pub severity: Severity,
    pub subject: String,
    pub title: String,
    pub detail: Option<String>,
    pub service: Option<String>,
    #[ts(type = "number")]
    pub opened_at: i64,
    /// The event that opened it.
    #[ts(type = "number")]
    pub event_id: i64,
    /// The peer it goes through (an uptime check of a site Pluto proxies to
    /// Jupiter). While that peer is unreachable, it's held back from
    /// notifications: the peer's own problem says it. Absent before 0.11.
    #[serde(default)]
    pub depends_on: Option<String>,
    /// Who acknowledged it. An acknowledged problem sends no more
    /// notifications; its recovery still goes out. Absent before 0.16.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub acked_by: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable, type = "number")]
    pub acked_at: Option<i64>,
}

/// `GET /v1/events/:id/logs`: the last lines of a container that crashed,
/// read just after. Never sent in a notification: logs can hold secrets.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct EventLogs {
    #[ts(type = "number")]
    pub event_id: i64,
    #[ts(type = "number")]
    pub captured_at: i64,
    pub lines: Vec<crate::types::LogLine>,
}

/// What a silence covers.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum SilenceScope {
    /// Everything on this node.
    Node,
    /// Events whose service is `target`.
    Service,
}

text_enum!(SilenceScope { Node => "node", Service => "service" });

/// Notifications held back for a while, for planned work. Events are still
/// logged; a problem still open when the silence ends is sent then.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct Silence {
    #[ts(type = "number")]
    pub id: i64,
    pub scope: SilenceScope,
    /// The service, for a `service` silence.
    pub target: Option<String>,
    /// Unix seconds; `None` until someone ends it.
    #[ts(type = "number | null")]
    pub until: Option<i64>,
    pub reason: Option<String>,
    pub created_by: String,
    #[ts(type = "number")]
    pub created_at: i64,
}

impl Silence {
    pub fn active(&self, now: i64) -> bool {
        self.until.is_none_or(|u| u > now)
    }

    /// Whether this silence covers an event about `service`.
    pub fn covers(&self, service: Option<&str>, now: i64) -> bool {
        self.active(now) &&
            (match self.scope {
                SilenceScope::Node => true,
                SilenceScope::Service => service.is_some() && self.target.as_deref() == service,
            })
    }
}

/// `POST /v1/silences`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct SilenceInput {
    pub scope: SilenceScope,
    pub target: Option<String>,
    /// How long; `None` until someone ends it.
    pub minutes: Option<u32>,
    pub reason: Option<String>,
}

/// `GET /v1/events`.
///
/// With `?after=<id>`, the events after it, oldest first: what a client
/// polling for new events appends. Otherwise the newest events (before
/// `?before=<id>` if given), newest first: a page of the timeline.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct EventsResponse {
    pub events: Vec<Event>,
    /// Every problem open now, whatever page was asked for.
    pub problems: Vec<Problem>,
    /// The newest event's id (0 for an empty log), so a client knows where
    /// to poll from even when it asked for an older page.
    #[ts(type = "number")]
    pub latest_id: i64,
    /// The page was cut at the limit: there are more events in the direction
    /// asked for.
    pub more: bool,
    /// Silences in force now. Absent before 0.16.
    #[serde(default)]
    pub silences: Vec<Silence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text form is what's stored, so it must match serde's exactly.
    #[test]
    fn text_names_match_serde() {
        for s in [Severity::Info, Severity::Warning, Severity::Error] {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
            assert_eq!(Severity::parse(s.as_str()), Some(s));
        }
        for c in [
            EventCategory::Container,
            EventCategory::Backup,
            EventCategory::Disk,
            EventCategory::Action,
            EventCategory::Wol,
            EventCategory::Agent,
            EventCategory::Uptime,
            EventCategory::Update,
        ] {
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{}\"", c.as_str()));
            assert_eq!(EventCategory::parse(c.as_str()), Some(c));
        }
        for p in [ProblemState::Opened, ProblemState::Resolved] {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.as_str()));
        }
        assert_eq!(Severity::parse("fatal"), None);
    }

    #[test]
    fn severities_order_by_how_much_they_matter() {
        assert!(Severity::Info < Severity::Warning && Severity::Warning < Severity::Error);
    }
}
