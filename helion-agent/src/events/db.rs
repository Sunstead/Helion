//! The event log's tables in `state.db` (migrations in `store.rs`).

use super::{ NewEvent, ProblemSpec, Resolution };
use crate::error::AgentError;
use helion_common::types::{
    Event,
    EventCategory,
    Problem,
    ProblemRef,
    ProblemState,
    Severity,
    EventLogs,
    LogLine,
    Silence,
    SilenceScope,
};
use rusqlite::{ params, Connection, OptionalExtension, Row };

const EVENT_COLUMNS: &str =
    "id, at, category, kind, severity, subject, title, detail, actor, service, problem_key, problem_state";

/// What a new problem is written with; `acked_*` start empty.
const PROBLEM_COLUMNS: &str =
    "key, category, kind, severity, subject, title, detail, service, opened_at, event_id, depends_on";

const PROBLEM_READ: &str =
    "key, category, kind, severity, subject, title, detail, service, opened_at, event_id, depends_on, acked_by, acked_at";

/// `None` for a row this build doesn't understand (written by a newer agent
/// that was then rolled back), which is skipped rather than failing the page.
fn row_to_event(r: &Row) -> rusqlite::Result<Option<Event>> {
    let category: String = r.get(2)?;
    let severity: String = r.get(4)?;
    let problem_key: Option<String> = r.get(10)?;
    let problem_state: Option<String> = r.get(11)?;
    let (Some(category), Some(severity)) = (EventCategory::parse(&category), Severity::parse(&severity)) else {
        return Ok(None);
    };
    Ok(
        Some(Event {
            id: r.get(0)?,
            at: r.get(1)?,
            category,
            kind: r.get(3)?,
            severity,
            subject: r.get(5)?,
            title: r.get(6)?,
            detail: r.get(7)?,
            actor: r.get(8)?,
            service: r.get(9)?,
            problem: problem_key.zip(problem_state.as_deref().and_then(ProblemState::parse)).map(
                |(key, state)| ProblemRef { key, state }
            ),
        })
    )
}

fn row_to_problem(r: &Row) -> rusqlite::Result<Option<Problem>> {
    let category: String = r.get(1)?;
    let severity: String = r.get(3)?;
    let (Some(category), Some(severity)) = (EventCategory::parse(&category), Severity::parse(&severity)) else {
        return Ok(None);
    };
    Ok(
        Some(Problem {
            key: r.get(0)?,
            category,
            kind: r.get(2)?,
            severity,
            subject: r.get(4)?,
            title: r.get(5)?,
            detail: r.get(6)?,
            service: r.get(7)?,
            opened_at: r.get(8)?,
            event_id: r.get(9)?,
            depends_on: r.get(10)?,
            acked_by: r.get(11)?,
            acked_at: r.get(12)?,
        })
    )
}

fn insert(conn: &Connection, at: i64, e: &NewEvent, problem: Option<(&str, ProblemState)>) -> rusqlite::Result<Event> {
    conn.execute(
        "INSERT INTO events (at, category, kind, severity, subject, title, detail, actor, service, problem_key, problem_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            at,
            e.category.as_str(),
            e.kind,
            e.severity.as_str(),
            e.subject,
            e.title,
            e.detail,
            e.actor,
            e.service,
            problem.map(|(k, _)| k),
            problem.map(|(_, s)| s.as_str())
        ]
    )?;
    Ok(Event {
        id: conn.last_insert_rowid(),
        at,
        category: e.category,
        kind: e.kind.to_string(),
        severity: e.severity,
        subject: e.subject.clone(),
        title: e.title.clone(),
        detail: e.detail.clone(),
        actor: e.actor.clone(),
        service: e.service.clone(),
        problem: problem.map(|(key, state)| ProblemRef { key: key.to_string(), state }),
    })
}

pub fn record(conn: &Connection, at: i64, e: &NewEvent) -> Result<Event, AgentError> {
    insert(conn, at, e, None).map_err(AgentError::internal)
}

/// Writes the opening event and the open problem together.
pub fn open(conn: &Connection, at: i64, p: &ProblemSpec) -> Result<(Event, Problem), AgentError> {
    let tx = conn.unchecked_transaction().map_err(AgentError::internal)?;
    let event = insert(&tx, at, &p.event(), Some((&p.key, ProblemState::Opened))).map_err(AgentError::internal)?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO problems ({PROBLEM_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
        params![
            p.key,
            p.category.as_str(),
            p.kind,
            p.severity.as_str(),
            p.subject,
            p.title,
            p.detail,
            p.service,
            at,
            event.id,
            p.depends_on
        ]
    ).map_err(AgentError::internal)?;
    tx.commit().map_err(AgentError::internal)?;
    let problem = Problem {
        key: p.key.clone(),
        category: p.category,
        kind: p.kind.to_string(),
        severity: p.severity,
        subject: p.subject.clone(),
        title: p.title.clone(),
        detail: p.detail.clone(),
        service: p.service.clone(),
        opened_at: at,
        event_id: event.id,
        depends_on: p.depends_on.clone(),
        acked_by: None,
        acked_at: None,
    };
    Ok((event, problem))
}

/// Writes the resolving event and closes the problem together. The event
/// takes the problem's category, subject, service and severity.
pub fn resolve(conn: &Connection, at: i64, problem: &Problem, r: &Resolution) -> Result<Event, AgentError> {
    let tx = conn.unchecked_transaction().map_err(AgentError::internal)?;
    let e = NewEvent {
        category: problem.category,
        kind: "resolved",
        severity: problem.severity,
        subject: problem.subject.clone(),
        title: r.title.clone(),
        detail: r.detail.clone(),
        actor: None,
        service: problem.service.clone(),
        logs_of: None,
    };
    let event = insert(&tx, at, &e, Some((&problem.key, ProblemState::Resolved))).map_err(AgentError::internal)?;
    tx.execute("DELETE FROM problems WHERE key = ?1", [&problem.key]).map_err(AgentError::internal)?;
    tx.commit().map_err(AgentError::internal)?;
    Ok(event)
}

pub fn problems(conn: &Connection) -> Result<Vec<Problem>, AgentError> {
    let mut stmt = conn
        .prepare_cached(&format!("SELECT {PROBLEM_READ} FROM problems ORDER BY opened_at, key"))
        .map_err(AgentError::internal)?;
    let rows = stmt.query_map([], row_to_problem).map_err(AgentError::internal)?;
    let rows: Vec<Option<Problem>> = rows.collect::<rusqlite::Result<_>>().map_err(AgentError::internal)?;
    Ok(rows.into_iter().flatten().collect())
}

/// Up to `limit` events, and whether more matched. `after` pages forward
/// (oldest first); otherwise backward from `before`, or from the newest
/// (newest first).
pub fn page(
    conn: &Connection,
    after: Option<i64>,
    before: Option<i64>,
    limit: usize
) -> Result<(Vec<Event>, bool), AgentError> {
    let (sql, bound) = match after {
        Some(after) => (format!("SELECT {EVENT_COLUMNS} FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2"), after),
        None =>
            (
                format!("SELECT {EVENT_COLUMNS} FROM events WHERE id < ?1 ORDER BY id DESC LIMIT ?2"),
                before.unwrap_or(i64::MAX),
            ),
    };
    let mut stmt = conn.prepare_cached(&sql).map_err(AgentError::internal)?;
    // One extra row says whether there is more without a second query.
    let rows = stmt
        .query_map(params![bound, (limit + 1) as i64], row_to_event)
        .map_err(AgentError::internal)?;
    let mut events: Vec<Option<Event>> = rows.collect::<rusqlite::Result<_>>().map_err(AgentError::internal)?;
    let more = events.len() > limit;
    events.truncate(limit);
    Ok((events.into_iter().flatten().collect(), more))
}

/// Acknowledges an open problem, or clears its acknowledgement with `None`.
pub fn ack(conn: &Connection, key: &str, by: Option<&str>, at: i64) -> Result<(), AgentError> {
    conn.execute(
        "UPDATE problems SET acked_by = ?2, acked_at = ?3 WHERE key = ?1",
        params![key, by, by.map(|_| at)]
    ).map_err(AgentError::internal)?;
    Ok(())
}

const SILENCE_COLUMNS: &str = "id, scope, target, until, reason, created_by, created_at";

/// Silences in force at `now`.
pub fn silences(conn: &Connection, now: i64) -> Result<Vec<Silence>, AgentError> {
    let mut stmt = conn
        .prepare_cached(
            &format!("SELECT {SILENCE_COLUMNS} FROM silences WHERE until IS NULL OR until > ?1 ORDER BY created_at, id")
        )
        .map_err(AgentError::internal)?;
    let rows = stmt
        .query_map([now], |r| {
            let scope: String = r.get(1)?;
            let Some(scope) = SilenceScope::parse(&scope) else {
                return Ok(None);
            };
            Ok(
                Some(Silence {
                    id: r.get(0)?,
                    scope,
                    target: r.get(2)?,
                    until: r.get(3)?,
                    reason: r.get(4)?,
                    created_by: r.get(5)?,
                    created_at: r.get(6)?,
                })
            )
        })
        .map_err(AgentError::internal)?;
    let rows: Vec<Option<Silence>> = rows.collect::<rusqlite::Result<_>>().map_err(AgentError::internal)?;
    Ok(rows.into_iter().flatten().collect())
}

pub fn add_silence(conn: &Connection, s: &Silence) -> Result<i64, AgentError> {
    conn.execute(
        "INSERT INTO silences (scope, target, until, reason, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![s.scope.as_str(), s.target, s.until, s.reason, s.created_by, s.created_at]
    ).map_err(AgentError::internal)?;
    Ok(conn.last_insert_rowid())
}

/// Ends a silence now. Kept as a row until pruned, so it's still on record.
pub fn end_silence(conn: &Connection, id: i64, now: i64) -> Result<bool, AgentError> {
    let changed = conn
        .execute(
            "UPDATE silences SET until = ?2 WHERE id = ?1 AND (until IS NULL OR until > ?2)",
            params![id, now]
        )
        .map_err(AgentError::internal)?;
    Ok(changed > 0)
}

/// Deletes silences that ended before `before`.
pub fn prune_silences(conn: &Connection, before: i64) -> Result<usize, AgentError> {
    conn.execute("DELETE FROM silences WHERE until IS NOT NULL AND until < ?1", [before]).map_err(
        AgentError::internal
    )
}

/// Keeps a crashed container's last lines with its event.
pub fn save_logs(conn: &Connection, event_id: i64, at: i64, lines: &[LogLine]) -> Result<(), AgentError> {
    let json = serde_json::to_string(lines).map_err(AgentError::internal)?;
    conn.execute(
        "INSERT OR REPLACE INTO event_logs (event_id, captured_at, lines) VALUES (?1, ?2, ?3)",
        params![event_id, at, json]
    ).map_err(AgentError::internal)?;
    Ok(())
}

pub fn logs(conn: &Connection, event_id: i64) -> Result<Option<EventLogs>, AgentError> {
    let row: Option<(i64, String)> = conn
        .query_row("SELECT captured_at, lines FROM event_logs WHERE event_id = ?1", [event_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(AgentError::internal)?;
    row.map(|(captured_at, json)| {
        let lines = serde_json::from_str(&json).map_err(AgentError::internal)?;
        Ok(EventLogs { event_id, captured_at, lines })
    }).transpose()
}

pub fn latest_id(conn: &Connection) -> Result<i64, AgentError> {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0)).map_err(AgentError::internal)
}

/// Deletes events older than `before` (unix seconds), except any that opened
/// a problem still open. Returns how many went.
pub fn prune(conn: &Connection, before: i64) -> Result<usize, AgentError> {
    let deleted = conn
        .execute("DELETE FROM events WHERE at < ?1 AND id NOT IN (SELECT event_id FROM problems)", [before])
        .map_err(AgentError::internal)?;
    conn.execute("DELETE FROM event_logs WHERE event_id NOT IN (SELECT id FROM events)", []).map_err(
        AgentError::internal
    )?;
    Ok(deleted)
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>, AgentError> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .map_err(AgentError::internal)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<(), AgentError> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value]
    ).map_err(AgentError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn event(title: &str) -> NewEvent {
        NewEvent {
            category: EventCategory::Container,
            kind: "crashed",
            severity: Severity::Error,
            subject: "gitea".into(),
            title: title.into(),
            detail: None,
            actor: None,
            service: Some("gitea".into()),
            logs_of: None,
        }
    }

    fn spec(key: &str) -> ProblemSpec {
        ProblemSpec {
            key: key.into(),
            category: EventCategory::Disk,
            kind: "disk_warning",
            severity: Severity::Warning,
            subject: "/".into(),
            title: "/ is 86% full".into(),
            detail: Some("360 GB of 418 GB used".into()),
            service: None,
            depends_on: None,
        }
    }

    #[test]
    fn events_round_trip_and_page_both_ways() {
        let store = Store::in_memory();
        store.with(|c| {
            for i in 1..=5 {
                record(c, 100 + i, &event(&format!("e{i}"))).unwrap();
            }
            assert_eq!(latest_id(c).unwrap(), 5);

            let (newest, more) = page(c, None, None, 2).unwrap();
            assert_eq!(newest.iter().map(|e| e.id).collect::<Vec<_>>(), [5, 4]);
            assert!(more);

            let (older, more) = page(c, None, Some(2), 10).unwrap();
            assert_eq!(older.iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
            assert!(!more);

            let (after, more) = page(c, Some(3), None, 10).unwrap();
            assert_eq!(after.iter().map(|e| e.id).collect::<Vec<_>>(), [4, 5]);
            assert!(!more);
            assert_eq!(after[0].title, "e4");
            assert_eq!(after[0].service.as_deref(), Some("gitea"));
            assert_eq!(after[0].problem, None);
        });
    }

    #[test]
    fn opening_and_resolving_writes_both_sides() {
        let store = Store::in_memory();
        store.with(|c| {
            let (opened, problem) = open(c, 100, &spec("disk:/:warning")).unwrap();
            assert_eq!(opened.problem, Some(ProblemRef { key: "disk:/:warning".into(), state: ProblemState::Opened }));
            assert_eq!(problem.event_id, opened.id);
            assert_eq!(problems(c).unwrap(), vec![problem.clone()]);

            let resolution = Resolution { key: problem.key.clone(), title: "/ is below 80% again".into(), detail: None };
            let resolved = resolve(c, 200, &problem, &resolution).unwrap();
            assert_eq!(resolved.kind, "resolved");
            assert_eq!(resolved.severity, Severity::Warning, "a resolution carries its problem's severity");
            assert_eq!(resolved.category, EventCategory::Disk);
            assert_eq!(resolved.problem.unwrap().state, ProblemState::Resolved);
            assert!(problems(c).unwrap().is_empty());
        });
    }

    #[test]
    fn pruning_keeps_what_an_open_problem_points_at() {
        let store = Store::in_memory();
        store.with(|c| {
            record(c, 10, &event("old")).unwrap();
            let (opened, _) = open(c, 20, &spec("disk:/:warning")).unwrap();
            record(c, 500, &event("new")).unwrap();

            assert_eq!(prune(c, 100).unwrap(), 1);
            let (left, _) = page(c, None, None, 10).unwrap();
            assert_eq!(left.iter().map(|e| e.id).collect::<Vec<_>>(), [3, opened.id]);
        });
    }

    #[test]
    fn rows_from_a_newer_agent_are_skipped() {
        let store = Store::in_memory();
        store.with(|c| {
            record(c, 1, &event("known")).unwrap();
            c.execute(
                "INSERT INTO events (at, category, kind, severity, subject, title) VALUES (2, 'health', 'x', 'info', 's', 't')",
                []
            ).unwrap();
            let (events, _) = page(c, None, None, 10).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].title, "known");
        });
    }

    #[test]
    fn meta_values_overwrite() {
        let store = Store::in_memory();
        store.with(|c| {
            assert_eq!(meta_get(c, "version").unwrap(), None);
            meta_set(c, "version", "0.4.0").unwrap();
            meta_set(c, "version", "0.5.0").unwrap();
            assert_eq!(meta_get(c, "version").unwrap().as_deref(), Some("0.5.0"));
        });
    }

    #[test]
    fn an_acknowledgement_is_stored_and_cleared() {
        let store = Store::in_memory();
        store.with(|c| open(c, 10, &spec("disk:/:disk_warning"))).unwrap();
        store.with(|c| ack(c, "disk:/:disk_warning", Some("Pat"), 20)).unwrap();
        let p = &store.with(problems).unwrap()[0];
        assert_eq!((p.acked_by.as_deref(), p.acked_at), (Some("Pat"), Some(20)));
        store.with(|c| ack(c, "disk:/:disk_warning", None, 30)).unwrap();
        assert_eq!(store.with(problems).unwrap()[0].acked_by, None);
    }

    #[test]
    fn silences_are_listed_while_in_force_and_ended_ones_pruned() {
        let store = Store::in_memory();
        let s = |scope, target: Option<&str>, until| Silence {
            id: 0,
            scope,
            target: target.map(Into::into),
            until,
            reason: None,
            created_by: "Pat".into(),
            created_at: 0,
        };
        let forever = store.with(|c| add_silence(c, &s(SilenceScope::Node, None, None))).unwrap();
        let hour = store.with(|c| add_silence(c, &s(SilenceScope::Service, Some("web"), Some(3_600)))).unwrap();
        assert_eq!(store.with(|c| silences(c, 100)).unwrap().len(), 2);
        assert_eq!(store.with(|c| silences(c, 3_600)).unwrap().len(), 1, "ended at its hour");

        assert!(store.with(|c| end_silence(c, forever, 200)).unwrap());
        assert!(!store.with(|c| end_silence(c, forever, 300)).unwrap(), "already ended");
        assert_eq!(store.with(|c| silences(c, 300)).unwrap().iter().map(|s| s.id).collect::<Vec<_>>(), [hour]);
        assert_eq!(store.with(|c| prune_silences(c, 4_000)).unwrap(), 2);
    }
}
