//! The event log: an append-only record of what happened on this node, and
//! the problems that are open now.
//!
//! Detectors report what they see and never decide what's new:
//!
//! - [`Report::Event`] for something that happened once ("riley restarted gitea").
//! - [`Report::Open`] for a condition that is true now ("gitea is unhealthy").
//!   Repeating it while it stays true is expected and free.
//! - [`Report::Resolve`] for a condition that has cleared.
//!
//! One task owns the open problems and turns reports into rows, so opening an
//! open problem or resolving a closed one does nothing. A detector only
//! resolves when it has seen the good condition, which gives hysteresis (a disk
//! warns at 85% and clears below 80%) and makes restarts safe: a problem opened
//! before a restart stays open until its detector sees it clear.
//!
//! Every stored event is broadcast for the notifier. Reports are sent with
//! `try_send`, so a detector never waits on SQLite.

pub mod backups;
pub mod containers;
pub mod db;
pub mod disks;
pub mod lifecycle;
pub mod metrics;

use crate::{ config::EventsConfig, docker::DockerHandle, sample::host::unix_now, store::Store };
use helion_common::types::{ Event, EventCategory, LogLine, Problem, Severity, Silence };
use std::{
    collections::HashMap,
    sync::{ atomic::{ AtomicBool, Ordering }, Arc },
    time::Duration,
};
use tokio::sync::{ broadcast, mpsc, oneshot, watch };

/// Something that happened once.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub category: EventCategory,
    pub kind: &'static str,
    pub severity: Severity,
    pub subject: String,
    pub title: String,
    pub detail: Option<String>,
    pub actor: Option<String>,
    pub service: Option<String>,
    /// A container whose last lines to keep with the event, read once it's
    /// stored: a crash's evidence, before a recreate takes it away.
    pub logs_of: Option<String>,
}

impl NewEvent {
    pub fn new(category: EventCategory, kind: &'static str, severity: Severity, subject: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            category,
            kind,
            severity,
            subject: subject.into(),
            title: title.into(),
            detail: None,
            actor: None,
            service: None,
            logs_of: None,
        }
    }

    pub fn logs_of(mut self, container_id: impl Into<String>) -> Self {
        self.logs_of = Some(container_id.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn service(mut self, service: Option<String>) -> Self {
        self.service = service;
        self
    }
}

/// A condition a detector has seen become true.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemSpec {
    /// Identifies the condition across reports and restarts.
    pub key: String,
    pub category: EventCategory,
    pub kind: &'static str,
    pub severity: Severity,
    pub subject: String,
    pub title: String,
    pub detail: Option<String>,
    pub service: Option<String>,
    /// A peer whose being unreachable would explain it: while it is, the
    /// notifier holds this back.
    pub depends_on: Option<String>,
}

impl ProblemSpec {
    /// The event that records it opening.
    fn event(&self) -> NewEvent {
        NewEvent {
            category: self.category,
            kind: self.kind,
            severity: self.severity,
            subject: self.subject.clone(),
            title: self.title.clone(),
            detail: self.detail.clone(),
            actor: None,
            service: self.service.clone(),
            logs_of: None,
        }
    }
}

/// A condition a detector has seen clear.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub key: String,
    pub title: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Event(NewEvent),
    Open(ProblemSpec),
    Resolve(Resolution),
}

/// Edits from the API, answered once the task has stored them, so a read
/// right after reflects them.
enum Command {
    Ack {
        key: String,
        /// `None` clears the acknowledgement.
        by: Option<String>,
        done: oneshot::Sender<Result<(), crate::error::AgentError>>,
    },
    Silence {
        silence: Silence,
        done: oneshot::Sender<Result<Silence, crate::error::AgentError>>,
    },
    EndSilence {
        id: i64,
        done: oneshot::Sender<Result<(), crate::error::AgentError>>,
    },
}

/// Reports can arrive in bursts (a deploy recreating every container), but
/// never faster than SQLite writes them for long.
const QUEUE: usize = 512;
/// Stored events waiting for the notifier.
const BROADCAST: usize = 256;

#[derive(Clone)]
pub struct EventsHandle {
    tx: mpsc::Sender<Report>,
    commands: mpsc::Sender<Command>,
    pub store: Store,
    /// The problems open now, republished on every change.
    pub problems: watch::Receiver<Arc<[Problem]>>,
    /// Silences not yet pruned; check `Silence::active` before trusting one.
    pub silences: watch::Receiver<Arc<[Silence]>>,
    stored: broadcast::Sender<Arc<Event>>,
    dropped: Arc<AtomicBool>,
}

impl EventsHandle {
    pub fn report(&self, report: Report) {
        match self.tx.try_send(report) {
            Ok(()) => self.dropped.store(false, Ordering::Relaxed),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Warn once per run of drops, not once per report.
                if !self.dropped.swap(true, Ordering::Relaxed) {
                    tracing::warn!("event log queue full; dropping reports");
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    pub fn record(&self, event: NewEvent) {
        self.report(Report::Event(event));
    }

    pub fn report_all(&self, reports: impl IntoIterator<Item = Report>) {
        for r in reports {
            self.report(r);
        }
    }

    pub fn open_problems(&self) -> Arc<[Problem]> {
        self.problems.borrow().clone()
    }

    /// Every event as it's stored.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.stored.subscribe()
    }

    /// Silences in force at `now`.
    pub fn active_silences(&self, now: i64) -> Vec<Silence> {
        self.silences
            .borrow()
            .iter()
            .filter(|s| s.active(now))
            .cloned()
            .collect()
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, crate::error::AgentError>>) -> Command
    ) -> Result<T, crate::error::AgentError> {
        let gone = || crate::error::AgentError::Unavailable("the event log is unavailable".into());
        let (done, wait) = oneshot::channel();
        self.commands.send(make(done)).await.map_err(|_| gone())?;
        wait.await.map_err(|_| gone())?
    }

    /// Acknowledges an open problem (`by`), or clears that (`None`).
    pub async fn ack(&self, key: &str, by: Option<&str>) -> Result<(), crate::error::AgentError> {
        let (key, by) = (key.to_string(), by.map(str::to_string));
        self.ask(|done| Command::Ack { key, by, done }).await
    }

    pub async fn silence(&self, silence: Silence) -> Result<Silence, crate::error::AgentError> {
        self.ask(|done| Command::Silence { silence, done }).await
    }

    pub async fn end_silence(&self, id: i64) -> Result<(), crate::error::AgentError> {
        self.ask(|done| Command::EndSilence { id, done }).await
    }
}

/// The most lines kept with a crash, and the most they may take together.
const LOG_LINES: u32 = 100;
const LOG_BYTES: usize = 64 * 1024;
/// One line longer than this is cut, so a single huge line can't push out
/// every other.
const LOG_LINE_BYTES: usize = 4 * 1024;

/// Keeps the newest lines that fit in [`LOG_BYTES`], each cut to
/// [`LOG_LINE_BYTES`].
fn cap_lines(lines: Vec<LogLine>) -> Vec<LogLine> {
    let mut kept: Vec<LogLine> = Vec::new();
    let mut size = 0;
    for mut line in lines.into_iter().rev() {
        if line.text.len() > LOG_LINE_BYTES {
            let mut end = LOG_LINE_BYTES;
            while !line.text.is_char_boundary(end) {
                end -= 1;
            }
            line.text.truncate(end);
        }
        size += line.text.len();
        if size > LOG_BYTES {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    kept
}

/// Reads a container's last lines and keeps them with the event. Best effort:
/// a container already removed (`--rm`) simply has none.
async fn capture_logs(store: Store, docker: DockerHandle, container: String, event_id: i64) {
    let Some(client) = docker.client() else {
        return;
    };
    let lines = match crate::api::logs::read_tail(&client, &container, LOG_LINES, 0).await {
        Ok(lines) if !lines.is_empty() => cap_lines(lines),
        Ok(_) => {
            return;
        }
        Err(e) => {
            tracing::debug!(error = %e, "no logs kept with the event");
            return;
        }
    };
    let at = unix_now();
    if let Err(e) = store.call(move |c| db::save_logs(c, event_id, at, &lines)).await {
        tracing::warn!(error = %e, "cannot keep logs with the event");
    }
}

/// Loads the open problems and starts the task that owns them. With `docker`,
/// a crash's last lines are kept with its event.
pub async fn spawn(
    cfg: &EventsConfig,
    store: Store,
    docker: Option<DockerHandle>
) -> Result<EventsHandle, crate::error::AgentError> {
    let open = store.call(db::problems).await?;
    let silenced = store.call(|c| db::silences(c, unix_now())).await?;
    let (tx, mut rx) = mpsc::channel::<Report>(QUEUE);
    let (commands, mut cmd_rx) = mpsc::channel::<Command>(16);
    let (problems_tx, problems) = watch::channel::<Arc<[Problem]>>(open.clone().into());
    let (silences_tx, silences) = watch::channel::<Arc<[Silence]>>(silenced.into());
    let (stored, _) = broadcast::channel(BROADCAST);
    let handle = EventsHandle {
        tx,
        commands,
        store: store.clone(),
        problems,
        silences,
        stored: stored.clone(),
        dropped: Arc::new(AtomicBool::new(false)),
    };

    let retain = Duration::from_secs(u64::from(cfg.retain_days) * 86_400);
    tokio::spawn(async move {
        let mut open: HashMap<String, Problem> = open.into_iter().map(|p| (p.key.clone(), p)).collect();
        let mut prune = tokio::time::interval(Duration::from_secs(3_600));
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let publish = |open: &HashMap<String, Problem>| {
            let mut list: Vec<Problem> = open.values().cloned().collect();
            list.sort_by(|a, b| (a.opened_at, &a.key).cmp(&(b.opened_at, &b.key)));
            let _ = problems_tx.send(list.into());
        };

        loop {
            tokio::select! {
                report = rx.recv() => {
                    let Some(report) = report else { return; };
                    let logs_of = match &report {
                        Report::Event(e) => e.logs_of.clone(),
                        _ => None,
                    };
                    let Some((event, changed)) = apply(&store, &mut open, report).await else { continue; };
                    if let (Some(container), Some(docker)) = (logs_of, &docker) {
                        tokio::spawn(capture_logs(store.clone(), docker.clone(), container, event.id));
                    }
                    if changed {
                        publish(&open);
                    }
                    // No receivers is normal: the notifier may be off.
                    let _ = stored.send(Arc::new(event));
                }
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return; };
                    match cmd {
                        Command::Ack { key, by, done } => {
                            let result = if let Some(p) = open.get_mut(&key) {
                                let at = unix_now();
                                let (k, b) = (key.clone(), by.clone());
                                let stored = store.call(move |c| db::ack(c, &k, b.as_deref(), at)).await;
                                if stored.is_ok() {
                                    p.acked_at = by.as_ref().map(|_| at);
                                    p.acked_by = by;
                                    publish(&open);
                                }
                                stored
                            } else {
                                Err(crate::error::AgentError::NotFound(format!("no open problem {key}")))
                            };
                            let _ = done.send(result);
                        }
                        Command::Silence { mut silence, done } => {
                            let s = silence.clone();
                            let result = store.call(move |c| db::add_silence(c, &s)).await.map(|id| {
                                silence.id = id;
                                silence
                            });
                            reload_silences(&store, &silences_tx).await;
                            let _ = done.send(result);
                        }
                        Command::EndSilence { id, done } => {
                            let now = unix_now();
                            let result = match store.call(move |c| db::end_silence(c, id, now)).await {
                                Ok(true) => Ok(()),
                                Ok(false) => Err(crate::error::AgentError::NotFound(format!("no silence {id} in force"))),
                                Err(e) => Err(e),
                            };
                            reload_silences(&store, &silences_tx).await;
                            let _ = done.send(result);
                        }
                    }
                }
                _ = prune.tick() => {
                    let before = unix_now() - retain.as_secs() as i64;
                    match store.call(move |c| db::prune(c, before)).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(deleted = n, "pruned old events"),
                        Err(e) => tracing::warn!(error = %e, "cannot prune events"),
                    }
                    // Ended silences stay a day, then go.
                    let ended = unix_now() - 86_400;
                    if let Err(e) = store.call(move |c| db::prune_silences(c, ended)).await {
                        tracing::warn!(error = %e, "cannot prune silences");
                    }
                    reload_silences(&store, &silences_tx).await;
                }
            }
        }
    });

    Ok(handle)
}

async fn reload_silences(store: &Store, tx: &watch::Sender<Arc<[Silence]>>) {
    match store.call(|c| db::silences(c, unix_now())).await {
        Ok(list) => {
            let _ = tx.send(list.into());
        }
        Err(e) => tracing::warn!(error = %e, "cannot read silences"),
    }
}

/// Stores one report. Returns the stored event, and whether the open problems
/// changed; `None` when there was nothing to do or the write failed. A failed
/// write leaves the in-memory state alone, so the detector's next report
/// tries again.
async fn apply(store: &Store, open: &mut HashMap<String, Problem>, report: Report) -> Option<(Event, bool)> {
    let at = unix_now();
    let result = match report {
        Report::Event(e) => store.call(move |c| db::record(c, at, &e)).await.map(|ev| (ev, false)),
        Report::Open(p) => {
            if open.contains_key(&p.key) {
                return None;
            }
            match store.call(move |c| db::open(c, at, &p)).await {
                Ok((event, problem)) => {
                    open.insert(problem.key.clone(), problem);
                    Ok((event, true))
                }
                Err(e) => Err(e),
            }
        }
        Report::Resolve(r) => {
            let problem = open.get(&r.key)?.clone();
            match store.call(move |c| db::resolve(c, at, &problem, &r)).await {
                Ok(event) => {
                    if let Some(p) = &event.problem {
                        open.remove(&p.key);
                    }
                    Ok((event, true))
                }
                Err(e) => Err(e),
            }
        }
    };
    match result {
        Ok(stored) => Some(stored),
        Err(e) => {
            tracing::warn!(error = %e, "cannot write to the event log");
            None
        }
    }
}

/// "1.6 TB", "412 MB": for titles, so a notification reads without the app.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "4m", "2h 5m": how long something took or lasted.
pub fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 if secs % 3_600 >= 60 => format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EventsConfig {
        EventsConfig::default()
    }

    fn line(text: &str) -> LogLine {
        LogLine { stream: helion_common::types::LogStream::Stdout, ts: None, text: text.into(), container: None }
    }

    #[test]
    fn kept_logs_are_the_newest_that_fit() {
        let lines: Vec<LogLine> = (0..40).map(|i| line(&format!("{i}{}", "x".repeat(3_000)))).collect();
        let kept = cap_lines(lines);
        assert!(kept.iter().map(|l| l.text.len()).sum::<usize>() <= LOG_BYTES);
        assert!(kept.last().unwrap().text.starts_with("39"), "the newest line survives");
        assert_eq!(kept.len(), 21);

        let huge = cap_lines(vec![line(&"é".repeat(10_000))]);
        assert!(huge[0].text.len() <= LOG_LINE_BYTES, "a huge line is cut, on a char boundary");
    }

    #[tokio::test]
    async fn acknowledging_and_silencing_go_through_the_task() {
        let store = Store::in_memory();
        let handle = spawn(&cfg(), store.clone(), None).await.unwrap();
        assert!(handle.ack("nope", Some("Pat")).await.is_err(), "only an open problem");

        handle.report(
            Report::Open(ProblemSpec {
                key: "disk:/:disk_warning".into(),
                category: EventCategory::Disk,
                kind: "disk_warning",
                severity: Severity::Warning,
                subject: "/".into(),
                title: "/ is 90% full".into(),
                detail: None,
                service: None,
                depends_on: None,
            })
        );
        let mut problems = handle.problems.clone();
        problems.wait_for(|p| !p.is_empty()).await.unwrap();
        handle.ack("disk:/:disk_warning", Some("Pat")).await.unwrap();
        assert_eq!(handle.open_problems()[0].acked_by.as_deref(), Some("Pat"));

        let s = handle
            .silence(helion_common::types::Silence {
                id: 0,
                scope: helion_common::types::SilenceScope::Node,
                target: None,
                until: None,
                reason: None,
                created_by: "Pat".into(),
                created_at: unix_now(),
            }).await
            .unwrap();
        assert_eq!(handle.active_silences(unix_now()).len(), 1);
        handle.end_silence(s.id).await.unwrap();
        assert!(handle.active_silences(unix_now() + 1).is_empty());
    }

    fn unhealthy() -> ProblemSpec {
        ProblemSpec {
            key: "container:gitea:unhealthy".into(),
            category: EventCategory::Container,
            kind: "unhealthy",
            severity: Severity::Warning,
            subject: "gitea".into(),
            title: "gitea is unhealthy".into(),
            detail: None,
            service: Some("gitea".into()),
            depends_on: None,
        }
    }

    fn resolution() -> Resolution {
        Resolution { key: "container:gitea:unhealthy".into(), title: "gitea is healthy again".into(), detail: None }
    }

    async fn next(rx: &mut broadcast::Receiver<Arc<Event>>) -> Option<Arc<Event>> {
        tokio::time::timeout(Duration::from_millis(500), rx.recv()).await.ok()?.ok()
    }

    #[tokio::test]
    async fn repeated_opens_and_resolves_store_one_event_each() {
        let store = Store::in_memory();
        let handle = spawn(&cfg(), store.clone(), None).await.unwrap();
        let mut rx = handle.subscribe();

        handle.report(Report::Resolve(resolution())); // nothing open: ignored
        for _ in 0..3 {
            handle.report(Report::Open(unhealthy()));
        }
        let opened = next(&mut rx).await.expect("the first open is stored");
        assert_eq!(opened.title, "gitea is unhealthy");

        for _ in 0..3 {
            handle.report(Report::Resolve(resolution()));
        }
        let resolved = next(&mut rx).await.expect("the first resolve is stored");
        assert_eq!(resolved.title, "gitea is healthy again");
        assert!(next(&mut rx).await.is_none(), "the repeats did nothing");

        let (events, _) = store.with(|c| db::page(c, None, None, 10)).unwrap();
        assert_eq!(events.len(), 2);
        assert!(handle.open_problems().is_empty());
    }

    #[tokio::test]
    async fn open_problems_survive_a_restart() {
        let store = Store::in_memory();
        let first = spawn(&cfg(), store.clone(), None).await.unwrap();
        let mut rx = first.subscribe();
        first.report(Report::Open(unhealthy()));
        next(&mut rx).await.unwrap();
        assert_eq!(first.open_problems().len(), 1);

        // A second engine over the same database, as after an agent restart.
        let second = spawn(&cfg(), store.clone(), None).await.unwrap();
        assert_eq!(second.open_problems()[0].key, "container:gitea:unhealthy");
        let mut rx = second.subscribe();
        second.report(Report::Open(unhealthy()));
        second.report(Report::Resolve(resolution()));
        let only = next(&mut rx).await.unwrap();
        assert_eq!(only.kind, "resolved", "the open after restart was a no-op");
    }

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(360_000_000_000), "360 GB");
        assert_eq!(human_bytes(1_600_000_000_000), "1.6 TB");
        assert_eq!(human_bytes(12_300_000), "12.3 MB");
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(840), "14m");
        assert_eq!(human_duration(7_500), "2h 5m");
        assert_eq!(human_duration(7_200), "2h");
        assert_eq!(human_duration(200_000), "2d 7h");
    }
}
