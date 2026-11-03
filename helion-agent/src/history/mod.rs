//! Metrics history: a small SQLite store the agent writes to and queries.
//!
//! Layout: the sampler hands rows to a dedicated writer thread over a bounded
//! channel, the writer batches them into one transaction per ~10s, and reads
//! go through a separate read-only connection. `rusqlite::Connection` is
//! `!Sync` and awkward to hold across an `.await`, so nothing here is async;
//! handlers call `query` from `spawn_blocking`.

pub mod containers;
pub mod rollup;
pub mod schema;

use crate::{ config::HistoryConfig, error::AgentError, sample::host::HostSample };
use containers::{ ContainerRow, Select };
use helion_common::types::{ ContainerSeries, MetricSeries, MetricStep };
use rusqlite::{ Connection, OpenFlags };
use std::{
    sync::{ atomic::{ AtomicBool, Ordering }, mpsc, Arc, Mutex },
    time::{ Duration, Instant },
};

/// One raw sample, flattened for storage.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    pub ts: i64,
    pub cpu_pct: f64,
    pub mem_used: i64,
    pub swap_used: i64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub load1: f64,
}

impl From<&HostSample> for Row {
    fn from(s: &HostSample) -> Self {
        Self {
            ts: s.sampled_at,
            cpu_pct: s.cpu_pct as f64,
            mem_used: s.mem_used_bytes as i64,
            swap_used: s.swap_used_bytes as i64,
            net_rx_bps: s.net_rx_bps,
            net_tx_bps: s.net_tx_bps,
            // Host-level disk IO is the sum across reported filesystems that
            // have counters. The per-disk breakdown stays in the live sample
            // only.
            disk_read_bps: s.disk
                .iter()
                .filter(|d| d.io_available)
                .map(|d| d.read_bps)
                .sum(),
            disk_write_bps: s.disk
                .iter()
                .filter(|d| d.io_available)
                .map(|d| d.write_bps)
                .sum(),
            load1: s.load1,
        }
    }
}

/// What the writer thread is sent.
enum Msg {
    Row(Row),
    /// One closed minute, every container in it. Written at once: it is
    /// already the batch.
    Containers(Vec<ContainerRow>),
    /// Write the batch now, then answer.
    Flush(mpsc::Sender<()>),
}

#[derive(Clone)]
pub struct HistoryHandle {
    tx: mpsc::SyncSender<Msg>,
    read: Arc<Mutex<Connection>>,
    warned: Arc<AtomicBool>,
    /// Whether per-container history is kept (`[history] containers`).
    containers: bool,
}

/// A likely cause for a failed open. SQLite only says "unable to open
/// database file", which hides the common case: a root-owned volume.
pub fn diagnose(path: &std::path::Path) -> &'static str {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return "check the path";
    };
    if !dir.is_dir() {
        return "the directory does not exist and could not be created";
    }
    let probe = dir.join(".helion-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            "the directory is writable; the file itself may be unreadable or corrupt"
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied =>
            "the directory is not writable by the agent's user; chown it (or the volume) to the agent's uid",
        Err(e) if e.raw_os_error() == Some(30) => "the filesystem is read-only; mount a writable volume there",
        Err(_) => "the directory is not writable",
    }
}

impl HistoryHandle {
    /// Opens the database, applies the schema, and spawns the writer thread.
    pub fn open(cfg: &HistoryConfig) -> rusqlite::Result<Self> {
        if let Some(parent) = cfg.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut writer = Connection::open(&cfg.path)?;
        schema::init(&writer)?;

        let read = Connection::open_with_flags(
            &cfg.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
        )?;
        read.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA temp_store = MEMORY;")?;

        // Bounded: if the writer ever wedges we drop samples rather than let
        // the queue grow without limit behind the live stream.
        let (tx, rx) = mpsc::sync_channel::<Msg>(512);
        let containers = cfg.containers;
        let cfg = cfg.clone();

        std::thread::Builder
            ::new()
            .name("history-writer".into())
            .spawn(move || writer_loop(&mut writer, rx, cfg))
            .expect("spawn history-writer");

        Ok(Self {
            tx,
            read: Arc::new(Mutex::new(read)),
            warned: Arc::new(AtomicBool::new(false)),
            containers,
        })
    }

    /// Called from the sampler thread. Never blocks: a backlogged writer costs
    /// us a metrics point, which is much better than stalling the live stream.
    pub fn push(&self, sample: &HostSample) {
        if self.tx.try_send(Msg::Row(Row::from(sample))).is_err() {
            // Warn once rather than every second for as long as it lasts.
            if !self.warned.swap(true, Ordering::Relaxed) {
                tracing::warn!("history writer backlogged; dropping metric samples");
            }
        } else {
            self.warned.store(false, Ordering::Relaxed);
        }
    }

    pub fn keeps_containers(&self) -> bool {
        self.containers
    }

    /// Called from the container sampler once a minute. Never blocks, like
    /// `push`.
    pub fn push_containers(&self, rows: Vec<ContainerRow>) {
        if !self.containers || rows.is_empty() {
            return;
        }
        if self.tx.try_send(Msg::Containers(rows)).is_err() {
            tracing::warn!("history writer backlogged; dropping a minute of container history");
        }
    }

    /// Writes the pending batch, waiting up to `timeout`. For shutdown: the
    /// sampler thread holds a handle for the life of the process, so the
    /// writer never sees its channel close.
    pub fn flush(&self, timeout: Duration) {
        let (done, wait) = mpsc::channel();
        // A full queue means the writer is already stuck.
        if self.tx.try_send(Msg::Flush(done)).is_err() || wait.recv_timeout(timeout).is_err() {
            tracing::warn!("history writer didn't flush in time");
        }
    }

    pub fn query(
        &self,
        from: i64,
        to: i64,
        step: MetricStep,
        max_points: usize
    ) -> Result<MetricSeries, AgentError> {
        if to <= from {
            return Err(AgentError::BadRequest("`to` must be after `from`".into()));
        }
        let max_points = max_points.clamp(1, 20_000);
        let step_secs = resolve_step(step, to - from, max_points);

        let conn = self.read.lock().map_err(|_| {
            AgentError::Unavailable("history database is unavailable".into())
        })?;
        read_series(&conn, from, to, step_secs).map_err(AgentError::internal)
    }

    pub fn query_containers(
        &self,
        from: i64,
        to: i64,
        step: MetricStep,
        max_points: usize,
        select: &Select
    ) -> Result<ContainerSeries, AgentError> {
        if !self.containers {
            return Err(AgentError::NotEnabled("container history"));
        }
        if to <= from {
            return Err(AgentError::BadRequest("`to` must be after `from`".into()));
        }
        let max_points = max_points.clamp(1, 20_000);
        // No 1s tier for containers: a request for one gets minutes.
        let step_secs = match resolve_step(step, to - from, max_points) {
            1 | 60 => 60,
            _ => 300,
        };
        let conn = self.read.lock().map_err(|_| {
            AgentError::Unavailable("history database is unavailable".into())
        })?;
        containers::read(&conn, from, to, step_secs, select).map_err(AgentError::internal)
    }
}

/// Picks the finest tier whose point count fits inside `max_points`.
fn resolve_step(step: MetricStep, span: i64, max_points: usize) -> u32 {
    match step {
        MetricStep::OneSec => 1,
        MetricStep::OneMin => 60,
        MetricStep::FiveMin => 300,
        MetricStep::Auto => {
            let fits = |secs: i64| (span / secs) as usize <= max_points;
            if fits(1) {
                1
            } else if fits(60) {
                60
            } else {
                300
            }
        }
    }
}

fn read_series(
    conn: &Connection,
    from: i64,
    to: i64,
    step_secs: u32
) -> rusqlite::Result<MetricSeries> {
    // The raw tier has no avg/max distinction and exactly one sample per row;
    // selecting the same column twice keeps one result shape for all tiers.
    let sql = match step_secs {
        1 =>
            "SELECT ts, cpu_pct, cpu_pct, mem_used, swap_used,
                    net_rx_bps, net_rx_bps, net_tx_bps, net_tx_bps,
                    disk_read_bps, disk_read_bps, disk_write_bps, disk_write_bps,
                    load1, 1
             FROM metrics_1s WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts",
        60 =>
            "SELECT ts, cpu_avg, cpu_max, mem_avg, swap_avg,
                    net_rx_avg, net_rx_max, net_tx_avg, net_tx_max,
                    disk_read_avg, disk_read_max, disk_write_avg, disk_write_max,
                    load1_avg, n
             FROM metrics_1m WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts",
        _ =>
            "SELECT ts, cpu_avg, cpu_max, mem_avg, swap_avg,
                    net_rx_avg, net_rx_max, net_tx_avg, net_tx_max,
                    disk_read_avg, disk_read_max, disk_write_avg, disk_write_max,
                    load1_avg, n
             FROM metrics_5m WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts",
    };

    let mut series = MetricSeries {
        step_secs,
        from,
        to,
        ts: Vec::new(),
        cpu_pct: Vec::new(),
        cpu_pct_max: Vec::new(),
        mem_used_bytes: Vec::new(),
        swap_used_bytes: Vec::new(),
        net_rx_bps: Vec::new(),
        net_rx_bps_max: Vec::new(),
        net_tx_bps: Vec::new(),
        net_tx_bps_max: Vec::new(),
        disk_read_bps: Vec::new(),
        disk_read_bps_max: Vec::new(),
        disk_write_bps: Vec::new(),
        disk_write_bps_max: Vec::new(),
        load1: Vec::new(),
        n: Vec::new(),
    };

    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(rusqlite::params![from, to])?;
    while let Some(r) = rows.next()? {
        series.ts.push(r.get(0)?);
        series.cpu_pct.push(r.get::<_, f64>(1)? as f32);
        series.cpu_pct_max.push(r.get::<_, f64>(2)? as f32);
        series.mem_used_bytes.push(r.get(3)?);
        series.swap_used_bytes.push(r.get(4)?);
        series.net_rx_bps.push(r.get(5)?);
        series.net_rx_bps_max.push(r.get(6)?);
        series.net_tx_bps.push(r.get(7)?);
        series.net_tx_bps_max.push(r.get(8)?);
        series.disk_read_bps.push(r.get(9)?);
        series.disk_read_bps_max.push(r.get(10)?);
        series.disk_write_bps.push(r.get(11)?);
        series.disk_write_bps_max.push(r.get(12)?);
        series.load1.push(r.get(13)?);
        series.n.push(r.get::<_, i64>(14)? as u32);
    }

    Ok(series)
}

/// Batches incoming rows and runs the rollup/retention jobs.
///
/// Flushes on whichever comes first: a full batch, the flush interval, or
/// shutdown. With WAL + `synchronous = NORMAL` this is roughly six disk
/// touches a minute rather than sixty.
fn writer_loop(conn: &mut Connection, rx: mpsc::Receiver<Msg>, cfg: HistoryConfig) {
    const MAX_BATCH: usize = 60;
    /// Rows reach the writer within a second or so of their timestamp.
    const ROLLUP_LAG_SECS: i64 = 5;
    /// A container minute is written only once the next one has started, a
    /// sampler tick (2 s by default) after it closes, or later if Docker is
    /// slow to answer.
    const CONTAINER_ROLLUP_LAG_SECS: i64 = 90;
    let flush_every = Duration::from_secs(cfg.flush_interval_secs.max(1));

    let mut batch: Vec<Row> = Vec::with_capacity(MAX_BATCH);
    let mut last_flush = Instant::now();
    let mut last_rollup = Instant::now();
    let mut last_retain = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Msg::Row(row)) => batch.push(row),
            Ok(Msg::Containers(rows)) => {
                if let Err(e) = containers::write(conn, &rows) {
                    tracing::warn!(error = %e, rows = rows.len(), "failed to write container history");
                }
            }
            Ok(Msg::Flush(done)) => {
                flush(conn, &mut batch);
                last_flush = Instant::now();
                let _ = done.send(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Every sender dropped: the agent is shutting down. Flush what we
            // have so `docker compose down` doesn't lose the batch.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                flush(conn, &mut batch);
                tracing::debug!("history writer stopped");
                return;
            }
        }

        if batch.len() >= MAX_BATCH || last_flush.elapsed() >= flush_every {
            flush(conn, &mut batch);
            last_flush = Instant::now();
        }

        let now = crate::sample::host::unix_now();

        if last_rollup.elapsed() >= Duration::from_secs(60) {
            last_rollup = Instant::now();
            // A rollup never revisits a bucket, so everything for it must be
            // on disk first: the batch still holding the minute's last
            // seconds, and rows still on their way in (hence the lag).
            flush(conn, &mut batch);
            last_flush = Instant::now();
            let closed_by = now - ROLLUP_LAG_SECS;
            if let Err(e) = rollup::rollup_1m(conn, closed_by) {
                tracing::warn!(error = %e, "1m rollup failed");
            }
            if let Err(e) = rollup::rollup_5m(conn, closed_by) {
                tracing::warn!(error = %e, "5m rollup failed");
            }
            if cfg.containers {
                if let Err(e) = containers::rollup_5m(conn, now - CONTAINER_ROLLUP_LAG_SECS) {
                    tracing::warn!(error = %e, "container 5m rollup failed");
                }
            }
        }

        if last_retain.elapsed() >= Duration::from_secs(3600) {
            last_retain = Instant::now();
            let trimmed = rollup
                ::retain(conn, now, cfg.retain_1s_secs, cfg.retain_1m_secs, cfg.retain_5m_secs)
                .and_then(|host| {
                    // Kept when `containers` is off too, so turning it off
                    // lets the old rows age out instead of staying forever.
                    containers
                        ::retain(
                            conn,
                            now,
                            cfg.retain_containers_1m_secs,
                            cfg.retain_containers_5m_secs
                        )
                        .map(|c| host + c)
                });
            match trimmed {
                Ok(deleted) if deleted > 0 => {
                    tracing::debug!(deleted, "trimmed metrics history");
                    // Stop the -wal file creeping after a bulk delete.
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "retention pass failed"),
            }
        }
    }
}

fn flush(conn: &mut Connection, batch: &mut Vec<Row>) {
    if batch.is_empty() {
        return;
    }
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO metrics_1s
                 (ts, cpu_pct, mem_used, swap_used, net_rx_bps, net_tx_bps,
                  disk_read_bps, disk_write_bps, load1)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            )?;
            for r in batch.iter() {
                stmt.execute(
                    rusqlite::params![
                        r.ts,
                        r.cpu_pct,
                        r.mem_used,
                        r.swap_used,
                        r.net_rx_bps,
                        r.net_tx_bps,
                        r.disk_read_bps,
                        r.disk_write_bps,
                        r.load1
                    ]
                )?;
            }
        }
        tx.commit()
    })();

    if let Err(e) = result {
        tracing::warn!(error = %e, rows = batch.len(), "failed to write metrics batch");
    }
    batch.clear();
}

#[cfg(test)]
mod diagnose_tests {
    use super::diagnose;

    #[test]
    fn reports_missing_and_writable_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(diagnose(&dir.path().join("metrics.db")).contains("writable;"));
        assert!(diagnose(&dir.path().join("nope/metrics.db")).contains("does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_an_unwritable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let hint = diagnose(&locked.join("metrics.db"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores permissions, so only assert when the probe was refused.
        if !hint.contains("writable;") {
            assert!(hint.contains("not writable by the agent's user"), "{hint}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_step_picks_the_finest_tier_that_fits() {
        // 10 minutes at 1s = 600 points, comfortably under the cap.
        assert_eq!(resolve_step(MetricStep::Auto, 600, 1000), 1);
        // 24 hours: 86400 at 1s and 1440 at 1m both blow the cap; 288 at 5m fits.
        assert_eq!(resolve_step(MetricStep::Auto, 86_400, 1000), 300);
        // 1 hour: 3600 at 1s is too many, 60 at 1m fits.
        assert_eq!(resolve_step(MetricStep::Auto, 3_600, 1000), 60);
    }

    #[test]
    fn explicit_step_is_honoured_regardless_of_span() {
        assert_eq!(resolve_step(MetricStep::OneSec, 86_400, 10), 1);
        assert_eq!(resolve_step(MetricStep::FiveMin, 60, 10_000), 300);
    }

    #[test]
    fn disk_io_sums_only_disks_with_counters() {
        use helion_common::types::{ DiskInfo, DiskKind };
        let disk = |read_bps: f64, io_available: bool| DiskInfo {
            mount: "/".into(),
            label: "/".into(),
            used_bytes: 0,
            total_bytes: 1,
            read_bps,
            write_bps: read_bps * 2.0,
            io_available,
            kind: DiskKind::Ssd,
        };
        let sample = HostSample {
            cpu_pct: 0.0,
            cpu_per_core: vec![],
            mem_used_bytes: 0,
            swap_used_bytes: 0,
            load1: 0.0,
            load5: 0.0,
            load15: 0.0,
            disk: vec![disk(100.0, true), disk(7.0, false), disk(10.0, true)],
            nets: vec![],
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            uptime_secs: 0,
            sampled_at: 0,
            temps: vec![],
        };
        let row = Row::from(&sample);
        assert_eq!((row.disk_read_bps, row.disk_write_bps), (110.0, 220.0));
    }

    #[test]
    fn flush_writes_the_pending_batch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = HistoryConfig { path: dir.path().join("history.db"), ..HistoryConfig::default() };
        let h = HistoryHandle::open(&cfg).unwrap();
        let now = crate::sample::host::unix_now();
        for i in 0..3 {
            let row = Row {
                ts: now - 3 + i,
                cpu_pct: 10.0,
                mem_used: 1,
                swap_used: 0,
                net_rx_bps: 0.0,
                net_tx_bps: 0.0,
                disk_read_bps: 0.0,
                disk_write_bps: 0.0,
                load1: 0.0,
            };
            h.tx.try_send(Msg::Row(row)).unwrap();
        }
        assert!(h.query(now - 10, now, MetricStep::OneSec, 100).unwrap().ts.is_empty(), "still batched");
        h.flush(Duration::from_secs(5));
        assert_eq!(h.query(now - 10, now, MetricStep::OneSec, 100).unwrap().ts.len(), 3);
    }

    #[test]
    fn query_returns_parallel_arrays_of_equal_length() {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();
        for i in 0..10 {
            conn.execute(
                "INSERT INTO metrics_1s VALUES (?1, 50.0, 100, 0, 1.0, 2.0, 3.0, 4.0, 0.1)",
                [i]
            ).unwrap();
        }

        let s = read_series(&conn, 0, 9, 1).unwrap();
        assert_eq!(s.ts.len(), 10);
        // Every column must line up with `ts` or the client's zip silently
        // misaligns the chart.
        assert_eq!(s.cpu_pct.len(), 10);
        assert_eq!(s.cpu_pct_max.len(), 10);
        assert_eq!(s.mem_used_bytes.len(), 10);
        assert_eq!(s.net_rx_bps.len(), 10);
        assert_eq!(s.disk_write_bps_max.len(), 10);
        assert_eq!(s.n.len(), 10);
        assert_eq!(s.step_secs, 1);
        assert!(s.n.iter().all(|&n| n == 1), "raw rows represent one sample each");
    }

    #[test]
    fn range_is_inclusive_and_ordered() {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();
        for i in [5i64, 1, 3, 9, 7] {
            conn.execute(
                "INSERT INTO metrics_1s VALUES (?1, 1.0, 1, 0, 0.0, 0.0, 0.0, 0.0, 0.0)",
                [i]
            ).unwrap();
        }
        let s = read_series(&conn, 3, 7, 1).unwrap();
        assert_eq!(s.ts, vec![3, 5, 7]);
    }
}
