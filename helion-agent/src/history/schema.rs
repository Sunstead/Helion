//! SQLite schema and pragmas for the metrics store.

use rusqlite::Connection;

/// WAL is not optional here: range queries run from HTTP handlers while the
/// writer thread is flushing and the rollup job is running. Under the default
/// rollback journal those would serialise and surface as `SQLITE_BUSY`.
///
/// `synchronous = NORMAL` with WAL means commits don't fsync at all — the
/// fsync happens at checkpoint. A process crash is safe; a power cut may cost
/// the last few seconds of metrics, which is a fine trade for a graph.
///
/// `temp_store = MEMORY` is what lets the container run with `read_only: true`
/// — otherwise SQLite tries to write temp files and fails.
pub const PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA busy_timeout = 5000;
    PRAGMA temp_store = MEMORY;
    PRAGMA wal_autocheckpoint = 1000;
    PRAGMA foreign_keys = OFF;
";

/// `ts INTEGER PRIMARY KEY` makes the timestamp the rowid, so range scans are
/// ordered table scans with no secondary index to maintain.
///
/// The rolled-up tiers carry avg *and* max so a zoomed-out chart can't hide a
/// spike, plus `n` so the UI can tell a gap from a genuine zero.
const DDL: &str = "
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS metrics_1s (
        ts            INTEGER PRIMARY KEY,
        cpu_pct       REAL    NOT NULL,
        mem_used      INTEGER NOT NULL,
        swap_used     INTEGER NOT NULL,
        net_rx_bps    REAL    NOT NULL,
        net_tx_bps    REAL    NOT NULL,
        disk_read_bps  REAL   NOT NULL,
        disk_write_bps REAL   NOT NULL,
        load1         REAL    NOT NULL
    );

    CREATE TABLE IF NOT EXISTS metrics_1m (
        ts                 INTEGER PRIMARY KEY,
        cpu_avg            REAL NOT NULL,
        cpu_max            REAL NOT NULL,
        mem_avg            REAL NOT NULL,
        swap_avg           REAL NOT NULL,
        net_rx_avg         REAL NOT NULL,
        net_rx_max         REAL NOT NULL,
        net_tx_avg         REAL NOT NULL,
        net_tx_max         REAL NOT NULL,
        disk_read_avg      REAL NOT NULL,
        disk_read_max      REAL NOT NULL,
        disk_write_avg     REAL NOT NULL,
        disk_write_max     REAL NOT NULL,
        load1_avg          REAL NOT NULL,
        n                  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS metrics_5m (
        ts                 INTEGER PRIMARY KEY,
        cpu_avg            REAL NOT NULL,
        cpu_max            REAL NOT NULL,
        mem_avg            REAL NOT NULL,
        swap_avg           REAL NOT NULL,
        net_rx_avg         REAL NOT NULL,
        net_rx_max         REAL NOT NULL,
        net_tx_avg         REAL NOT NULL,
        net_tx_max         REAL NOT NULL,
        disk_read_avg      REAL NOT NULL,
        disk_read_max      REAL NOT NULL,
        disk_write_avg     REAL NOT NULL,
        disk_write_max     REAL NOT NULL,
        load1_avg          REAL NOT NULL,
        n                  INTEGER NOT NULL
    );
";

/// Per-container history (0.15). Keyed by name, which survives a Compose
/// recreate, through a small dictionary so each row carries an integer.
///
/// `(ts, series)` as the key: every read is a time range, and the "top N"
/// query wants every container in it. There is no 1s tier; the live table
/// covers the last minutes. The IO columns are nullable because a container
/// on the host network has no traffic of its own.
const CONTAINERS: &str = "
    CREATE TABLE container_series (
        id        INTEGER PRIMARY KEY,
        name      TEXT    NOT NULL UNIQUE,
        service   TEXT,
        last_seen INTEGER NOT NULL
    );

    CREATE TABLE containers_1m (
        ts        INTEGER NOT NULL,
        series    INTEGER NOT NULL,
        cpu_avg   REAL    NOT NULL,
        cpu_max   REAL    NOT NULL,
        mem_avg   REAL    NOT NULL,
        mem_max   REAL    NOT NULL,
        net_rx    REAL,
        net_tx    REAL,
        blk_read  REAL,
        blk_write REAL,
        n         INTEGER NOT NULL,
        PRIMARY KEY (ts, series)
    ) WITHOUT ROWID;

    CREATE TABLE containers_5m (
        ts        INTEGER NOT NULL,
        series    INTEGER NOT NULL,
        cpu_avg   REAL    NOT NULL,
        cpu_max   REAL    NOT NULL,
        mem_avg   REAL    NOT NULL,
        mem_max   REAL    NOT NULL,
        net_rx    REAL,
        net_tx    REAL,
        blk_read  REAL,
        blk_write REAL,
        n         INTEGER NOT NULL,
        PRIMARY KEY (ts, series)
    ) WITHOUT ROWID;
";

/// Append only: never edit a migration that has shipped. The first is the
/// schema from before there were migrations, `IF NOT EXISTS` throughout, so a
/// database an older agent made adopts it unchanged. An older agent opening a
/// newer database only runs that first one, and ignores tables it doesn't know.
const MIGRATIONS: &[&str] = &[DDL, CONTAINERS];

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(PRAGMAS)?;
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i + 1)?;
        tx.commit()?;
    }
    Ok(())
}

pub fn get_cursor(conn: &Connection, key: &str) -> i64 {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

pub fn set_cursor(conn: &Connection, key: &str, value: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value.to_string()]
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> usize {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_database_from_before_migrations_keeps_its_rows_and_gains_the_new_tables() {
        let conn = Connection::open_in_memory().unwrap();
        // What 0.14 and earlier did: the DDL, no version.
        conn.execute_batch(DDL).unwrap();
        conn.execute("INSERT INTO metrics_1s VALUES (1, 5.0, 1, 0, 0, 0, 0, 0, 0)", []).unwrap();

        init(&conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
        let kept: i64 = conn.query_row("SELECT count(*) FROM metrics_1s", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1);
        conn.execute("INSERT INTO container_series (name, last_seen) VALUES ('a', 1)", []).unwrap();
    }

    #[test]
    fn init_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len());
    }
}
