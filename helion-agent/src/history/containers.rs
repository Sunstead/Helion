//! Per-container history: which container used what, and when.
//!
//! The container sampler folds each 2 s snapshot into a [`MinuteAccumulator`]
//! and hands the writer one batch per closed minute, so the disk sees a single
//! transaction a minute however many containers there are. From there it is
//! the host's scheme again: closed buckets roll up into 5 minutes, weighted by
//! sample count, and retention never deletes what a rollup hasn't consumed.

use super::schema::{ get_cursor, set_cursor };
use helion_common::types::{ ContainerHistory, ContainerInfo, ContainerRank, ContainerSeries };
use rusqlite::{ Connection, OptionalExtension };
use std::collections::{ BTreeMap, HashMap };

pub const CURSOR_5M: &str = "rollup_containers_5m_cursor";

/// One container's closed minute.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerRow {
    /// The minute's start, unix seconds.
    pub ts: i64,
    pub name: String,
    pub service: Option<String>,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub mem_avg: f64,
    pub mem_max: f64,
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
    pub blk_read: Option<f64>,
    pub blk_write: Option<f64>,
    pub n: u32,
}

/// An average over the samples that had a value.
#[derive(Debug, Default, Clone, Copy)]
struct Mean {
    sum: f64,
    n: u32,
}

impl Mean {
    fn add(&mut self, v: Option<f64>) {
        if let Some(v) = v {
            self.sum += v;
            self.n += 1;
        }
    }

    fn get(self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / (self.n as f64))
    }
}

#[derive(Debug, Default)]
struct Minute {
    service: Option<String>,
    cpu_sum: f64,
    cpu_max: f64,
    mem_sum: f64,
    mem_max: f64,
    net_rx: Mean,
    net_tx: Mean,
    blk_read: Mean,
    blk_write: Mean,
    n: u32,
}

/// Folds snapshots into per-container minutes. Pure: the caller supplies the
/// time, so the boundary handling is testable.
#[derive(Debug, Default)]
pub struct MinuteAccumulator {
    minute: i64,
    open: HashMap<String, Minute>,
}

impl MinuteAccumulator {
    /// Adds one snapshot taken at `at` (unix seconds). Returns the previous
    /// minute's rows once a snapshot from a later minute arrives. Only running
    /// containers count: a stopped one has no usage, and a gap says so better
    /// than a row of zeros.
    pub fn add(&mut self, at: i64, containers: &[ContainerInfo]) -> Option<Vec<ContainerRow>> {
        let minute = at.div_euclid(60) * 60;
        let closed = if minute > self.minute && !self.open.is_empty() {
            Some(self.drain())
        } else {
            None
        };
        // A clock stepping backwards must not file samples under a minute
        // that was already written.
        if minute < self.minute {
            return closed;
        }
        self.minute = minute;

        for c in containers.iter().filter(|c| c.state == "running") {
            let m = self.open.entry(c.name.clone()).or_default();
            let (cpu, mem) = (c.cpu_pct as f64, c.mem_used_bytes as f64);
            m.service.clone_from(&c.helion_service);
            m.cpu_sum += cpu;
            m.cpu_max = m.cpu_max.max(cpu);
            m.mem_sum += mem;
            m.mem_max = m.mem_max.max(mem);
            m.net_rx.add(c.net_rx_bps);
            m.net_tx.add(c.net_tx_bps);
            m.blk_read.add(c.blk_read_bps);
            m.blk_write.add(c.blk_write_bps);
            m.n += 1;
        }
        closed
    }

    fn drain(&mut self) -> Vec<ContainerRow> {
        let ts = self.minute;
        let mut rows: Vec<ContainerRow> = self.open
            .drain()
            .map(|(name, m)| {
                let n = m.n.max(1) as f64;
                ContainerRow {
                    ts,
                    name,
                    service: m.service,
                    cpu_avg: m.cpu_sum / n,
                    cpu_max: m.cpu_max,
                    mem_avg: m.mem_sum / n,
                    mem_max: m.mem_max,
                    net_rx: m.net_rx.get(),
                    net_tx: m.net_tx.get(),
                    blk_read: m.blk_read.get(),
                    blk_write: m.blk_write.get(),
                    n: m.n,
                }
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }
}

/// Writes one minute's rows in a single transaction.
pub fn write(conn: &mut Connection, rows: &[ContainerRow]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut series = tx.prepare_cached(
            "INSERT INTO container_series (name, service, last_seen) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET service = excluded.service, last_seen = excluded.last_seen
             RETURNING id"
        )?;
        let mut insert = tx.prepare_cached(
            "INSERT OR REPLACE INTO containers_1m
             (ts, series, cpu_avg, cpu_max, mem_avg, mem_max, net_rx, net_tx, blk_read, blk_write, n)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        )?;
        for r in rows {
            let id: i64 = series.query_row(rusqlite::params![r.name, r.service, r.ts], |row|
                row.get(0)
            )?;
            insert.execute(
                rusqlite::params![
                    r.ts,
                    id,
                    r.cpu_avg,
                    r.cpu_max,
                    r.mem_avg,
                    r.mem_max,
                    r.net_rx,
                    r.net_tx,
                    r.blk_read,
                    r.blk_write,
                    r.n
                ]
            )?;
        }
    }
    tx.commit()
}

/// Aggregates closed minutes into closed 5-minute buckets. The IO columns are
/// averaged over the minutes that had them; a bucket with none stays NULL
/// (SQLite's division by zero, through `nullif`).
pub fn rollup_5m(conn: &mut Connection, now: i64) -> rusqlite::Result<usize> {
    let closed = (now / 300) * 300;
    let cursor = get_cursor(conn, CURSOR_5M);
    if closed <= cursor {
        return Ok(0);
    }

    let weighted = |col: &str| {
        format!(
            "sum({col} * n) / nullif(sum(CASE WHEN {col} IS NULL THEN 0 ELSE n END), 0)"
        )
    };
    let sql = format!(
        "INSERT OR IGNORE INTO containers_5m
            (ts, series, cpu_avg, cpu_max, mem_avg, mem_max, net_rx, net_tx, blk_read, blk_write, n)
         SELECT (ts / 300) * 300, series,
                sum(cpu_avg * n) / sum(n), max(cpu_max),
                sum(mem_avg * n) / sum(n), max(mem_max),
                {}, {}, {}, {}, sum(n)
         FROM containers_1m
         WHERE ts >= ?1 AND ts < ?2
         GROUP BY (ts / 300) * 300, series",
        weighted("net_rx"),
        weighted("net_tx"),
        weighted("blk_read"),
        weighted("blk_write")
    );

    let tx = conn.transaction()?;
    let rows = tx.execute(&sql, rusqlite::params![cursor, closed])?;
    set_cursor(&tx, CURSOR_5M, closed)?;
    tx.commit()?;
    Ok(rows)
}

/// Trims both tiers, and forgets containers no tier still holds.
pub fn retain(
    conn: &mut Connection,
    now: i64,
    retain_1m: i64,
    retain_5m: i64
) -> rusqlite::Result<usize> {
    let safe_1m = (now - retain_1m).min(get_cursor(conn, CURSOR_5M));
    let tx = conn.transaction()?;
    let mut deleted = tx.execute("DELETE FROM containers_1m WHERE ts < ?1", [safe_1m])?;
    deleted += tx.execute("DELETE FROM containers_5m WHERE ts < ?1", [now - retain_5m])?;
    deleted += tx.execute(
        "DELETE FROM container_series WHERE last_seen < ?1
           AND id NOT IN (SELECT series FROM containers_1m)
           AND id NOT IN (SELECT series FROM containers_5m)",
        [now - retain_5m.max(retain_1m)]
    )?;
    tx.commit()?;
    Ok(deleted)
}

/// Which containers a query returns.
#[derive(Debug, Clone)]
pub enum Select {
    Names(Vec<String>),
    /// The `n` that used most over the range, by the average of each bucket's
    /// peak: a container that ran hot all day outranks one brief spike, and a
    /// spike still counts for more than its average would.
    Top(ContainerRank, usize),
}

/// The most containers one query returns.
pub const MAX_CONTAINERS: usize = 20;

pub fn read(
    conn: &Connection,
    from: i64,
    to: i64,
    step_secs: u32,
    select: &Select
) -> rusqlite::Result<ContainerSeries> {
    let table = if step_secs == 60 { "containers_1m" } else { "containers_5m" };

    let ids: Vec<(i64, String, Option<String>)> = match select {
        Select::Top(rank, n) => {
            let column = match rank {
                ContainerRank::Cpu => "cpu_max",
                ContainerRank::Mem => "mem_max",
            };
            let sql = format!(
                "SELECT s.id, s.name, s.service FROM {table} c
                 JOIN container_series s ON s.id = c.series
                 WHERE c.ts >= ?1 AND c.ts <= ?2
                 GROUP BY c.series ORDER BY avg(c.{column}) DESC, s.name LIMIT ?3"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let limit = (*n).clamp(1, MAX_CONTAINERS) as i64;
            let top = stmt
                .query_map(rusqlite::params![from, to, limit], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<_, _>>()?;
            top
        }
        Select::Names(names) => {
            let mut stmt = conn.prepare_cached(
                "SELECT id, name, service FROM container_series WHERE name = ?1"
            )?;
            let mut found = Vec::new();
            for name in names.iter().take(MAX_CONTAINERS) {
                if let Some(row) = stmt.query_row([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()? {
                    found.push(row);
                }
            }
            found
        }
    };

    // One pass over the range for the chosen containers, then lay the rows
    // onto a shared time axis.
    let index: HashMap<i64, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| (*id, i))
        .collect();
    type Bucket = [Option<f64>; 8];
    let mut buckets: BTreeMap<i64, Vec<(usize, Bucket)>> = BTreeMap::new();
    if !ids.is_empty() {
        let sql = format!(
            "SELECT ts, series, cpu_avg, cpu_max, mem_avg, mem_max, net_rx, net_tx, blk_read, blk_write
             FROM {table} WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(rusqlite::params![from, to])?;
        while let Some(r) = rows.next()? {
            let Some(&i) = index.get(&r.get::<_, i64>(1)?) else {
                continue;
            };
            let mut values: Bucket = [None; 8];
            for (k, v) in values.iter_mut().enumerate() {
                *v = r.get(k + 2)?;
            }
            buckets.entry(r.get(0)?).or_default().push((i, values));
        }
    }

    let len = buckets.len();
    let mut containers: Vec<ContainerHistory> = ids
        .into_iter()
        .map(|(_, name, service)| ContainerHistory {
            name,
            service,
            cpu_pct: vec![None; len],
            cpu_pct_max: vec![None; len],
            mem_used_bytes: vec![None; len],
            mem_used_bytes_max: vec![None; len],
            net_rx_bps: vec![None; len],
            net_tx_bps: vec![None; len],
            disk_read_bps: vec![None; len],
            disk_write_bps: vec![None; len],
        })
        .collect();
    let mut ts = Vec::with_capacity(len);
    for (slot, (t, entries)) in buckets.into_iter().enumerate() {
        ts.push(t);
        for (i, v) in entries {
            let c = &mut containers[i];
            c.cpu_pct[slot] = v[0];
            c.cpu_pct_max[slot] = v[1];
            c.mem_used_bytes[slot] = v[2];
            c.mem_used_bytes_max[slot] = v[3];
            c.net_rx_bps[slot] = v[4];
            c.net_tx_bps[slot] = v[5];
            c.disk_read_bps[slot] = v[6];
            c.disk_write_bps[slot] = v[7];
        }
    }

    Ok(ContainerSeries { step_secs, from, to, ts, containers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::schema;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();
        conn
    }

    fn container(name: &str, state: &str, cpu: f32, mem: u64) -> ContainerInfo {
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
            helion_service: Some("svc".into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            update_labels: Default::default(),
            cpu_pct: cpu,
            mem_used_bytes: mem,
            mem_limit_bytes: 0,
            net_rx_bps: None,
            net_tx_bps: None,
            blk_read_bps: None,
            blk_write_bps: None,
        }
    }

    fn row(ts: i64, name: &str, cpu: f64, n: u32) -> ContainerRow {
        ContainerRow {
            ts,
            name: name.into(),
            service: None,
            cpu_avg: cpu,
            cpu_max: cpu,
            mem_avg: 100.0,
            mem_max: 100.0,
            net_rx: None,
            net_tx: None,
            blk_read: None,
            blk_write: None,
            n,
        }
    }

    #[test]
    fn a_minute_is_handed_over_once_the_next_one_starts() {
        let mut acc = MinuteAccumulator::default();
        assert_eq!(acc.add(60, &[container("web", "running", 10.0, 100)]), None);
        let mut hot = container("web", "running", 30.0, 300);
        hot.net_rx_bps = Some(50.0);
        assert_eq!(acc.add(62, &[hot, container("db", "exited", 0.0, 0)]), None);

        let rows = acc.add(120, &[container("web", "running", 0.0, 0)]).expect("minute closed");
        assert_eq!(rows.len(), 1, "a stopped container gets no row");
        let r = &rows[0];
        assert_eq!((r.ts, r.n), (60, 2));
        assert_eq!((r.cpu_avg, r.cpu_max), (20.0, 30.0));
        assert_eq!((r.mem_avg, r.mem_max), (200.0, 300.0));
        // Averaged over the samples that had a value, not diluted by the one that didn't.
        assert_eq!(r.net_rx, Some(50.0));
        assert_eq!(r.service.as_deref(), Some("svc"));
    }

    #[test]
    fn a_clock_stepping_back_never_reopens_a_written_minute() {
        let mut acc = MinuteAccumulator::default();
        acc.add(120, &[container("web", "running", 10.0, 1)]);
        assert_eq!(acc.add(60, &[container("web", "running", 90.0, 1)]), None);
        let rows = acc.add(180, &[]).unwrap();
        assert_eq!((rows[0].ts, rows[0].n), (120, 1));
    }

    #[test]
    fn rows_roll_up_weighted_and_the_peak_survives() {
        let mut conn = db();
        let mut short = row(60, "web", 70.0, 6);
        short.net_rx = Some(10.0);
        write(&mut conn, &[row(0, "web", 10.0, 30), short]).unwrap();
        rollup_5m(&mut conn, 300).unwrap();

        let (avg, max, n, net): (f64, f64, i64, Option<f64>) = conn
            .query_row("SELECT cpu_avg, cpu_max, n, net_rx FROM containers_5m", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(n, 36);
        assert!((avg - 20.0).abs() < 1e-9, "(30 x 10 + 6 x 70) / 36, got {avg}");
        assert_eq!(max, 70.0);
        assert_eq!(net, Some(10.0), "only the minute that had traffic counts");
        // Idempotent: replaying adds nothing.
        assert_eq!(rollup_5m(&mut conn, 300).unwrap(), 0);
    }

    #[test]
    fn retention_waits_for_the_rollup_and_forgets_containers_with_no_rows() {
        let mut conn = db();
        write(&mut conn, &[row(0, "old", 1.0, 1)]).unwrap();
        retain(&mut conn, 10_000, 60, 100_000).unwrap();
        let kept: i64 = conn.query_row("SELECT count(*) FROM containers_1m", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "not rolled up yet");

        rollup_5m(&mut conn, 10_000).unwrap();
        retain(&mut conn, 10_000, 60, 100_000).unwrap();
        let kept: i64 = conn.query_row("SELECT count(*) FROM containers_1m", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 0);

        retain(&mut conn, 1_000_000, 60, 100_000).unwrap();
        let series: i64 = conn.query_row("SELECT count(*) FROM container_series", [], |r| r.get(0)).unwrap();
        assert_eq!(series, 0);
    }

    #[test]
    fn top_ranks_by_peak_and_shares_one_time_axis() {
        let mut conn = db();
        write(&mut conn, &[row(0, "busy", 80.0, 30), row(0, "idle", 1.0, 30)]).unwrap();
        write(&mut conn, &[row(60, "busy", 90.0, 30), row(60, "new", 5.0, 30)]).unwrap();

        let s = read(&conn, 0, 120, 60, &Select::Top(ContainerRank::Cpu, 2)).unwrap();
        assert_eq!(s.ts, vec![0, 60]);
        let names: Vec<_> = s.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["busy", "new"]);
        // `new` didn't exist in the first minute: a gap, not a zero.
        assert_eq!(s.containers[1].cpu_pct, vec![None, Some(5.0)]);
        assert!(s.containers.iter().all(|c| c.cpu_pct.len() == s.ts.len()));
    }

    #[test]
    fn names_select_only_those_containers() {
        let mut conn = db();
        write(&mut conn, &[row(0, "a", 1.0, 1), row(0, "b", 2.0, 1)]).unwrap();
        let s = read(&conn, 0, 60, 60, &Select::Names(vec!["b".into(), "gone".into()])).unwrap();
        assert_eq!(s.containers.len(), 1);
        assert_eq!(s.containers[0].cpu_pct, vec![Some(2.0)]);
    }
}
