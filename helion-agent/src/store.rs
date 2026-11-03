//! Settings edited from the UI, in their own small SQLite file.
//!
//! Separate from the metrics history: that database is owned by a writer
//! thread batching thousands of rows, and a slow settings write must never
//! hold up a flush (or the other way round). Writes here are rare and tiny,
//! so one connection behind a mutex, used from `spawn_blocking`, is enough.
//! Reads that serve requests come from samplers' snapshots, not from here.

use crate::error::AgentError;
use helion_common::types::{ WolProbe, WolTarget, WolWake };
use rusqlite::{ params, Connection, OptionalExtension };
use std::{ path::Path, sync::{ Arc, Mutex } };

/// Each entry runs once, in order, tracked by `PRAGMA user_version`. Append
/// only: never edit a migration that has shipped.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE wol_targets (
        id             INTEGER PRIMARY KEY,
        name           TEXT    NOT NULL UNIQUE COLLATE NOCASE,
        mac            TEXT    NOT NULL,
        broadcast      TEXT,
        port           INTEGER NOT NULL DEFAULT 9,
        tailnet_device TEXT,
        probe_host     TEXT,
        probe_port     INTEGER,
        wake_at        INTEGER,
        wake_by        TEXT,
        wake_woke      INTEGER,
        wake_took_secs INTEGER
    );",
    // 0.5: the event log (events/), with the problems open now and a few
    // markers the lifecycle check compares across restarts.
    "CREATE TABLE events (
        id            INTEGER PRIMARY KEY,
        at            INTEGER NOT NULL,
        category      TEXT    NOT NULL,
        kind          TEXT    NOT NULL,
        severity      TEXT    NOT NULL,
        subject       TEXT    NOT NULL,
        title         TEXT    NOT NULL,
        detail        TEXT,
        actor         TEXT,
        service       TEXT,
        problem_key   TEXT,
        problem_state TEXT
    );
    CREATE INDEX events_at ON events (at);
    CREATE TABLE problems (
        key       TEXT    PRIMARY KEY,
        category  TEXT    NOT NULL,
        kind      TEXT    NOT NULL,
        severity  TEXT    NOT NULL,
        subject   TEXT    NOT NULL,
        title     TEXT    NOT NULL,
        detail    TEXT,
        service   TEXT,
        opened_at INTEGER NOT NULL,
        event_id  INTEGER NOT NULL
    );
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
    // 0.5: where notifications go (notify/). `secret` is never sent back out.
    "CREATE TABLE notify_channels (
        id           INTEGER PRIMARY KEY,
        name         TEXT    NOT NULL UNIQUE COLLATE NOCASE,
        kind         TEXT    NOT NULL,
        url          TEXT    NOT NULL,
        topic        TEXT,
        secret       TEXT,
        enabled      INTEGER NOT NULL,
        min_severity TEXT    NOT NULL,
        recoveries   INTEGER NOT NULL,
        categories   TEXT    NOT NULL
    );",
    // 0.6: uptime checks (uptime/). Services get theirs from labels, so
    // only their settings are stored; results roll up by the hour.
    // AUTOINCREMENT so a new check never reuses a deleted one's id, and with
    // it that check's history.
    "CREATE TABLE uptime_checks (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        name          TEXT    NOT NULL UNIQUE COLLATE NOCASE,
        kind          TEXT    NOT NULL,
        target        TEXT    NOT NULL,
        interval_secs INTEGER,
        enabled       INTEGER NOT NULL,
        any_status    INTEGER NOT NULL
    );
    CREATE TABLE uptime_services (
        service    TEXT    PRIMARY KEY,
        enabled    INTEGER NOT NULL,
        path       TEXT,
        any_status INTEGER NOT NULL
    );
    CREATE TABLE uptime_hourly (
        check_id    TEXT    NOT NULL,
        hour        INTEGER NOT NULL,
        ok          INTEGER NOT NULL,
        fail        INTEGER NOT NULL,
        latency_sum INTEGER NOT NULL,
        latency_max INTEGER NOT NULL,
        PRIMARY KEY (check_id, hour)
    ) WITHOUT ROWID;",
    // 0.8: image updates (updates/). When each newer tag was first seen (the
    // minimum age before an automatic update), the policy per unit, and every
    // update and rollback, which resume after a restart: a deploy can
    // recreate the agent mid-run.
    "CREATE TABLE update_seen (
        repo       TEXT    NOT NULL,
        tag        TEXT    NOT NULL,
        first_seen INTEGER NOT NULL,
        PRIMARY KEY (repo, tag)
    ) WITHOUT ROWID;
    CREATE TABLE update_policy (
        unit   TEXT PRIMARY KEY,
        policy TEXT NOT NULL,
        paused TEXT
    );
    CREATE TABLE update_runs (
        id           TEXT    PRIMARY KEY,
        unit         TEXT    NOT NULL,
        kind         TEXT    NOT NULL,
        from_tag     TEXT    NOT NULL,
        to_tag       TEXT    NOT NULL,
        by           TEXT    NOT NULL,
        services     TEXT    NOT NULL,
        state        TEXT    NOT NULL,
        requested_at INTEGER NOT NULL,
        finished_at  INTEGER,
        run_id       INTEGER,
        run_url      TEXT,
        detail       TEXT,
        watch_until  INTEGER
    );
    CREATE INDEX update_runs_at ON update_runs (requested_at);",
    // 0.11: peers. A problem can depend on a peer, and a custom check can go
    // through one.
    "ALTER TABLE problems ADD COLUMN depends_on TEXT;
    ALTER TABLE uptime_checks ADD COLUMN via_peer TEXT;",
    // 0.15: services learned from what runs, so one that stops being there
    // is reported.
    "CREATE TABLE expected_services (
        service      TEXT    PRIMARY KEY,
        first_seen   INTEGER NOT NULL,
        last_running INTEGER NOT NULL,
        stopped_by   TEXT,
        stopped_at   INTEGER
    );",
];

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(mut conn: Connection) -> rusqlite::Result<Self> {
        // Same reasoning as the history database: WAL for concurrent reads,
        // temp_store in memory so a read-only rootfs is fine.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;"
        )?;
        migrate(&mut conn)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// Runs `f` on the connection off the async runtime.
    pub async fn call<T, F>(&self, f: F) -> Result<T, AgentError>
        where T: Send + 'static, F: FnOnce(&Connection) -> Result<T, AgentError> + Send + 'static
    {
        let conn = self.0.clone();
        tokio::task
            ::spawn_blocking(move || {
                let guard = conn.lock().unwrap_or_else(|p| p.into_inner());
                f(&guard)
            }).await
            .map_err(AgentError::internal)?
    }

    /// Blocking access, for tests.
    #[cfg(test)]
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        f(&guard)
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i + 1)?;
        tx.commit()?;
    }
    Ok(())
}

// --- Wake-on-LAN targets ----------------------------------------------------

/// A target with the outcome of its most recent wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTarget {
    pub target: WolTarget,
    pub last_wake: Option<WolWake>,
}

const TARGET_COLUMNS: &str =
    "id, name, mac, broadcast, port, tailnet_device, probe_host, probe_port,
     wake_at, wake_by, wake_woke, wake_took_secs";

fn row_to_target(r: &rusqlite::Row) -> rusqlite::Result<StoredTarget> {
    let id: i64 = r.get(0)?;
    let probe_host: Option<String> = r.get(6)?;
    let probe_port: Option<u16> = r.get(7)?;
    let wake_at: Option<i64> = r.get(8)?;
    Ok(StoredTarget {
        target: WolTarget {
            id: id.to_string(),
            name: r.get(1)?,
            mac: r.get(2)?,
            broadcast: r.get(3)?,
            port: r.get(4)?,
            tailnet_device: r.get(5)?,
            probe: probe_host.zip(probe_port).map(|(host, port)| WolProbe { host, port }),
        },
        last_wake: match wake_at {
            Some(at) =>
                Some(WolWake {
                    at,
                    by: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    woke: r.get(10)?,
                    took_secs: r.get(11)?,
                }),
            None => None,
        },
    })
}

fn map_unique(e: rusqlite::Error, name: &str) -> AgentError {
    match &e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            AgentError::BadRequest(format!("a target named {name} already exists")),
        _ => AgentError::internal(e),
    }
}

fn parse_id(id: &str) -> Result<i64, AgentError> {
    id.parse().map_err(|_| AgentError::NotFound(format!("no target {id}")))
}

pub fn list_targets(conn: &Connection) -> Result<Vec<StoredTarget>, AgentError> {
    let mut stmt = conn
        .prepare_cached(&format!("SELECT {TARGET_COLUMNS} FROM wol_targets ORDER BY name"))
        .map_err(AgentError::internal)?;
    let rows = stmt.query_map([], row_to_target).map_err(AgentError::internal)?;
    rows.collect::<rusqlite::Result<_>>().map_err(AgentError::internal)
}

pub fn get_target(conn: &Connection, id: &str) -> Result<StoredTarget, AgentError> {
    conn.query_row(
        &format!("SELECT {TARGET_COLUMNS} FROM wol_targets WHERE id = ?1"),
        [parse_id(id)?],
        row_to_target
    )
        .optional()
        .map_err(AgentError::internal)?
        .ok_or_else(|| AgentError::NotFound(format!("no target {id}")))
}

/// Inserts `t` (its `id` is ignored) and returns it with the new id.
pub fn insert_target(conn: &Connection, t: &WolTarget) -> Result<WolTarget, AgentError> {
    conn.execute(
        "INSERT INTO wol_targets (name, mac, broadcast, port, tailnet_device, probe_host, probe_port)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            t.name,
            t.mac,
            t.broadcast,
            t.port,
            t.tailnet_device,
            t.probe.as_ref().map(|p| &p.host),
            t.probe.as_ref().map(|p| p.port)
        ]
    ).map_err(|e| map_unique(e, &t.name))?;
    Ok(WolTarget { id: conn.last_insert_rowid().to_string(), ..t.clone() })
}

/// Replaces the settings of target `t.id`, keeping its wake history.
pub fn update_target(conn: &Connection, t: &WolTarget) -> Result<(), AgentError> {
    let changed = conn
        .execute(
            "UPDATE wol_targets SET name = ?2, mac = ?3, broadcast = ?4, port = ?5,
             tailnet_device = ?6, probe_host = ?7, probe_port = ?8 WHERE id = ?1",
            params![
                parse_id(&t.id)?,
                t.name,
                t.mac,
                t.broadcast,
                t.port,
                t.tailnet_device,
                t.probe.as_ref().map(|p| &p.host),
                t.probe.as_ref().map(|p| p.port)
            ]
        )
        .map_err(|e| map_unique(e, &t.name))?;
    if changed == 0 {
        return Err(AgentError::NotFound(format!("no target {}", t.id)));
    }
    Ok(())
}

pub fn delete_target(conn: &Connection, id: &str) -> Result<(), AgentError> {
    let changed = conn
        .execute("DELETE FROM wol_targets WHERE id = ?1", [parse_id(id)?])
        .map_err(AgentError::internal)?;
    if changed == 0 {
        return Err(AgentError::NotFound(format!("no target {id}")));
    }
    Ok(())
}

/// Records the latest wake, so its outcome survives an agent restart.
pub fn record_wake(conn: &Connection, id: &str, wake: &WolWake) -> Result<(), AgentError> {
    conn.execute(
        "UPDATE wol_targets SET wake_at = ?2, wake_by = ?3, wake_woke = ?4, wake_took_secs = ?5
         WHERE id = ?1",
        params![parse_id(id)?, wake.at, wake.by, wake.woke, wake.took_secs]
    ).map_err(AgentError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> WolTarget {
        WolTarget {
            id: String::new(),
            name: name.into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            broadcast: Some("192.168.1.255".into()),
            port: 9,
            tailnet_device: Some("nDESK".into()),
            probe: Some(WolProbe { host: "192.168.1.20".into(), port: 3389 }),
        }
    }

    #[test]
    fn round_trips_targets_and_wakes() {
        let store = Store::in_memory();
        store.with(|c| {
            let saved = insert_target(c, &target("desktop")).unwrap();
            assert!(!saved.id.is_empty());

            let listed = list_targets(c).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].target, saved);
            assert_eq!(listed[0].last_wake, None);

            let wake = WolWake { at: 100, by: "riley".into(), woke: Some(true), took_secs: Some(21) };
            record_wake(c, &saved.id, &wake).unwrap();

            // Editing settings keeps the wake history.
            update_target(c, &WolTarget { name: "Desktop PC".into(), probe: None, ..saved.clone() }).unwrap();
            let got = get_target(c, &saved.id).unwrap();
            assert_eq!(got.target.name, "Desktop PC");
            assert_eq!(got.target.probe, None);
            assert_eq!(got.last_wake, Some(wake));

            delete_target(c, &saved.id).unwrap();
            assert!(list_targets(c).unwrap().is_empty());
        });
    }

    #[test]
    fn names_are_unique_ignoring_case() {
        let store = Store::in_memory();
        store.with(|c| {
            insert_target(c, &target("desktop")).unwrap();
            let err = insert_target(c, &target("Desktop")).unwrap_err();
            assert!(matches!(err, AgentError::BadRequest(_)), "{err}");
        });
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let store = Store::in_memory();
        store.with(|c| {
            assert!(matches!(get_target(c, "7"), Err(AgentError::NotFound(_))));
            assert!(matches!(get_target(c, "nope"), Err(AgentError::NotFound(_))));
            assert!(matches!(delete_target(c, "7"), Err(AgentError::NotFound(_))));
        });
    }

    #[test]
    fn migrations_are_idempotent_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let s = Store::open(&path).unwrap();
            s.with(|c| insert_target(c, &target("desktop")).unwrap());
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.with(|c| list_targets(c).unwrap().len()), 1);
    }
}
