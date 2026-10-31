//! Uptime checks: an HTTP check for every service with a
//! `helion.service.url` label, plus the HTTP and TCP checks added in the UI.
//!
//! One task owns the checks, like Wake-on-LAN's: handlers read the published
//! snapshot, or send a reload and wait for it to republish. Probes run in
//! their own tasks and send their results back, so a slow target never holds
//! up the others or a reload. Results feed the event log (down after three
//! failures in a row, up after two passes) and hourly tallies in `state.db`
//! for the uptime figures; the newest 90 per check stay in memory for the
//! heartbeat bar.

pub mod cert;
pub mod db;
pub mod detector;
pub mod probe;

use crate::{
    backups::parse_rfc3339,
    config::UptimeConfig,
    error::AgentError,
    events::EventsHandle,
    sample::{ docker::ContainerSnapshot, host::unix_now },
    store::Store,
};
use cert::CertValidity;
use helion_common::types::{
    CertInfo,
    CheckKind,
    CheckSource,
    CheckState,
    ContainerInfo,
    UptimeBeat,
    UptimeCheck,
    UptimeCheckInput,
    UptimeEntry,
    UptimeResponse,
    UptimeServiceInput,
    UptimeStats,
};
use db::{ CustomCheck, Tally };
use detector::{ Streak, Subject };
use probe::{ Outcome, Prober };
use std::{
    collections::{ hash_map::DefaultHasher, HashMap, VecDeque },
    hash::{ Hash, Hasher },
    sync::Arc,
    time::Duration,
};
use tokio::{ sync::{ mpsc, oneshot, watch, Semaphore }, time::Instant };

/// Results kept per check for the heartbeat bar.
const RECENT: usize = 90;
const FLUSH_EVERY: Duration = Duration::from_secs(300);
const RETAIN_SECS: i64 = 90 * 86_400;
/// Probes running at once.
const CONCURRENCY: usize = 8;
pub const MIN_INTERVAL: u32 = 20;
pub const MAX_INTERVAL: u32 = 3_600;
/// Failures this soon after a service's container starts don't count: apps
/// answer 502 through the proxy while they boot, and a deploy restarts them.
const STARTUP_GRACE: i64 = 120;
/// A service whose containers vanish keeps its check, paused, this long:
/// a deploy that removes and recreates them isn't the service going away.
const VANISH_GRACE: i64 = 60;

pub struct UptimeSnapshot {
    pub json: Arc<str>,
    pub entries: Vec<UptimeEntry>,
}

enum Command {
    /// Re-read the stored checks. With a check id, run it now and answer once
    /// its result is in.
    Reload {
        run: Option<String>,
        done: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct UptimeHandle {
    pub store: Store,
    pub rx: watch::Receiver<Arc<UptimeSnapshot>>,
    pub default_interval: u32,
    /// `[[peers]]` names, which a custom check may go through.
    pub peers: Arc<[String]>,
    tx: mpsc::Sender<Command>,
}

impl UptimeHandle {
    /// Re-reads checks after an edit and waits for the new snapshot; with
    /// `run`, also for that check's first result.
    pub async fn reload(&self, run: Option<String>) -> Result<(), AgentError> {
        let (done, wait) = oneshot::channel();
        let stopped = || AgentError::Unavailable("uptime task stopped".into());
        self.tx.send(Command::Reload { run, done }).await.map_err(|_| stopped())?;
        wait.await.map_err(|_| stopped())
    }

    pub fn entry(&self, id: &str) -> Option<UptimeEntry> {
        self.rx
            .borrow()
            .entries.iter()
            .find(|e| e.check.id == id)
            .cloned()
    }
}

/// Checks a custom check's settings, filling in the default interval.
/// `peers` are the configured peers' names, which `via_peer` must be one of.
pub fn validate(input: UptimeCheckInput, id: i64, default_interval: u32, peers: &[String]) -> Result<CustomCheck, AgentError> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(AgentError::BadRequest("a name is 1 to 64 characters".into()));
    }
    let target = input.target.trim().to_string();
    match input.kind {
        CheckKind::Http => {
            let url = reqwest::Url
                ::parse(&target)
                .map_err(|_| AgentError::BadRequest(format!("{target} is not a URL")))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(AgentError::BadRequest("the URL must start with http:// or https://".into()));
            }
        }
        CheckKind::Tcp => {
            if probe::split_host_port(&target).is_none() {
                return Err(AgentError::BadRequest("a TCP check's target is host:port".into()));
            }
        }
    }
    if let Some(i) = input.interval_secs {
        if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&i) {
            return Err(
                AgentError::BadRequest(format!("the interval is {MIN_INTERVAL} to {MAX_INTERVAL} seconds"))
            );
        }
    }
    let via_peer = input.via_peer.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    if let Some(p) = &via_peer {
        if !peers.contains(p) {
            return Err(AgentError::BadRequest(format!("{p} isn't one of this agent's peers")));
        }
    }
    Ok(CustomCheck {
        id,
        name,
        kind: input.kind,
        target,
        interval_secs: input.interval_secs.filter(|&i| i != default_interval),
        enabled: input.enabled,
        any_status: input.any_status,
        via_peer,
    })
}

/// Checks a service check's settings.
pub fn validate_service(mut input: UptimeServiceInput) -> Result<UptimeServiceInput, AgentError> {
    input.path = input.path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty() && p != "/");
    if let Some(p) = &input.path {
        if !p.starts_with('/') || p.len() > 200 || p.contains(char::is_whitespace) {
            return Err(AgentError::BadRequest("the path starts with / and has no spaces".into()));
        }
    }
    Ok(input)
}

/// `https://` when the label has no scheme, as the app's `serviceHref` does.
fn service_url(label: &str, path: Option<&str>) -> String {
    let label = label.trim();
    let base = if label.contains("://") { label.to_string() } else { format!("https://{label}") };
    format!("{}{}", base.trim_end_matches('/'), path.unwrap_or("/"))
}

/// A check, and whether it can run now: a service's only runs while one of
/// its containers is running.
#[derive(Debug, Clone, PartialEq)]
struct Defined {
    check: UptimeCheck,
    active: bool,
    /// Unix seconds until which failures don't count.
    grace_until: Option<i64>,
}

/// One check per labelled service with a URL, then the custom checks.
fn define(
    containers: &[ContainerInfo],
    settings: &HashMap<String, UptimeServiceInput>,
    custom: &[CustomCheck],
    default_interval: u32
) -> Vec<Defined> {
    // (service, url, running, newest start of a running container)
    let mut services: Vec<(&str, Option<&str>, bool, Option<i64>)> = Vec::new();
    let mut sorted: Vec<&ContainerInfo> = containers.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for c in sorted {
        let Some(service) = c.helion_service.as_deref().filter(|s| *s != "system") else {
            continue;
        };
        let url = c.helion_service_url.as_deref().filter(|u| !u.trim().is_empty());
        let running = c.state == "running";
        let started = c.started_at
            .as_deref()
            .filter(|_| running)
            .and_then(parse_rfc3339);
        match services.iter_mut().find(|(s, ..)| *s == service) {
            Some(entry) => {
                entry.1 = entry.1.or(url);
                entry.2 |= running;
                entry.3 = entry.3.max(started);
            }
            None => services.push((service, url, running, started)),
        }
    }
    services.sort_by(|a, b| a.0.cmp(b.0));

    let mut out: Vec<Defined> = services
        .into_iter()
        .filter_map(|(service, url, running, started)| {
            let s = settings.get(service);
            Some(Defined {
                check: UptimeCheck {
                    id: format!("svc:{service}"),
                    source: CheckSource::Service,
                    name: service.to_string(),
                    kind: CheckKind::Http,
                    target: service_url(url?, s.and_then(|s| s.path.as_deref())),
                    interval_secs: default_interval,
                    enabled: s.is_none_or(|s| s.enabled),
                    any_status: s.is_some_and(|s| s.any_status),
                    service: Some(service.to_string()),
                    via_peer: None,
                },
                active: running,
                grace_until: started.map(|t| t + STARTUP_GRACE),
            })
        })
        .collect();
    out.extend(
        custom.iter().map(|c| Defined {
            check: UptimeCheck {
                id: c.id.to_string(),
                source: CheckSource::Custom,
                name: c.name.clone(),
                kind: c.kind,
                target: c.target.clone(),
                interval_secs: c.interval_secs.unwrap_or(default_interval),
                enabled: c.enabled,
                any_status: c.any_status,
                service: None,
                via_peer: c.via_peer.clone(),
            },
            active: true,
            grace_until: None,
        })
    );
    out
}

/// Keeps service checks whose containers just vanished, paused, for
/// [`VANISH_GRACE`]. Their state, heartbeat and open problem survive a
/// deploy that recreates the containers; one that stays gone is dropped
/// after that. `vanished` tracks since when, and forgets checks that came
/// back or were dropped.
fn carry_vanished(old: &[Defined], mut fresh: Vec<Defined>, vanished: &mut HashMap<String, i64>, now: i64) -> Vec<Defined> {
    let back: Vec<String> = fresh.iter().map(|c| c.check.id.clone()).collect();
    for o in old.iter().filter(|o| o.check.source == CheckSource::Service && !back.contains(&o.check.id)) {
        let since = *vanished.entry(o.check.id.clone()).or_insert(now);
        if now - since < VANISH_GRACE {
            fresh.push(Defined { active: false, grace_until: None, ..o.clone() });
        }
    }
    vanished.retain(|id, _| !back.contains(id) && fresh.iter().any(|c| &c.check.id == id));
    fresh
}

/// What the task keeps per check between results.
struct Live {
    streak: Streak,
    state: CheckState,
    since: Option<i64>,
    recent: VecDeque<UptimeBeat>,
    next: Instant,
    running: bool,
    cert: Option<CertValidity>,
}

impl Live {
    fn new(id: &str, interval: u32) -> Self {
        // Spread first runs across the interval, so a restart doesn't fire
        // every check at once.
        let mut h = DefaultHasher::new();
        id.hash(&mut h);
        let offset = Duration::from_millis(h.finish() % (u64::from(interval) * 1_000));
        Self {
            streak: Streak::default(),
            state: CheckState::Pending,
            since: None,
            recent: VecDeque::with_capacity(RECENT),
            next: Instant::now() + offset,
            running: false,
            cert: None,
        }
    }

    fn set_state(&mut self, state: CheckState, now: i64) {
        if self.state != state {
            self.state = state;
            self.since = Some(now);
        }
    }
}

struct ProbeDone {
    id: String,
    /// What was probed, so a result for a target since edited is ignored.
    target: String,
    at: i64,
    outcome: Outcome,
}

/// Hourly tallies not yet written, and the sums already stored.
#[derive(Default)]
struct Stats {
    pending: HashMap<(String, i64), Tally>,
    day: HashMap<String, Tally>,
    month: HashMap<String, Tally>,
    quarter: HashMap<String, Tally>,
}

impl Stats {
    fn record(&mut self, id: &str, at: i64, ok: bool, latency_ms: Option<u32>) {
        self.pending
            .entry((id.to_string(), at - at.rem_euclid(3_600)))
            .or_default()
            .add(ok, latency_ms);
    }

    fn for_check(&self, id: &str, now: i64) -> UptimeStats {
        let window = |stored: &HashMap<String, Tally>, secs: i64| {
            let mut t = stored.get(id).copied().unwrap_or_default();
            let from = hour_start(now - secs);
            for ((pid, hour), p) in &self.pending {
                if pid == id && *hour >= from {
                    t.merge(p);
                }
            }
            t
        };
        let fraction = |t: Tally| (!t.is_empty()).then(|| f64::from(t.ok) / f64::from(t.ok + t.fail));
        let day = window(&self.day, 86_400);
        UptimeStats {
            day: fraction(day),
            month: fraction(window(&self.month, 30 * 86_400)),
            quarter: fraction(window(&self.quarter, 90 * 86_400)),
            latency_ms: (day.ok > 0).then(|| (day.latency_sum / u64::from(day.ok)) as u32),
        }
    }
}

fn hour_start(t: i64) -> i64 {
    t - t.rem_euclid(3_600)
}

pub fn spawn(
    cfg: &UptimeConfig,
    store: Store,
    containers: watch::Receiver<Arc<ContainerSnapshot>>,
    events: Option<EventsHandle>,
    peers: Vec<String>
) -> UptimeHandle {
    let (snap_tx, rx) = watch::channel(
        Arc::new(UptimeSnapshot { json: r#"{"checks":[],"sampled_at":0}"#.into(), entries: vec![] })
    );
    let (tx, cmd_rx) = mpsc::channel::<Command>(16);
    let handle = UptimeHandle { store: store.clone(), rx, default_interval: cfg.interval_secs, peers: peers.into(), tx };
    let task = Task {
        store,
        events,
        prober: Prober::new(&cfg.local_domains),
        default_interval: cfg.interval_secs,
        containers,
        snap_tx,
        custom: Vec::new(),
        settings: HashMap::new(),
        checks: Vec::new(),
        vanished: HashMap::new(),
        live: HashMap::new(),
        certs: HashMap::new(),
        stats: Stats::default(),
        waiting: HashMap::new(),
        permits: Arc::new(Semaphore::new(CONCURRENCY)),
    };
    tokio::spawn(task.run(cmd_rx));
    handle
}

struct Task {
    store: Store,
    events: Option<EventsHandle>,
    prober: Prober,
    default_interval: u32,
    containers: watch::Receiver<Arc<ContainerSnapshot>>,
    snap_tx: watch::Sender<Arc<UptimeSnapshot>>,
    custom: Vec<CustomCheck>,
    settings: HashMap<String, UptimeServiceInput>,
    checks: Vec<Defined>,
    /// Service checks kept through a recreate, and since when.
    vanished: HashMap<String, i64>,
    live: HashMap<String, Live>,
    /// The certificate each HTTPS host last served.
    certs: HashMap<String, CertValidity>,
    stats: Stats,
    /// Reloads waiting for a check's result.
    waiting: HashMap<String, Vec<oneshot::Sender<()>>>,
    permits: Arc<Semaphore>,
}

impl Task {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<Command>) {
        let (done_tx, mut done_rx) = mpsc::channel::<ProbeDone>(64);
        self.load().await;
        self.refresh_stats().await;
        self.redefine();
        self.publish();

        let mut flush = tokio::time::interval_at(Instant::now() + FLUSH_EVERY, FLUSH_EVERY);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut flushes: u64 = 0;

        loop {
            self.start_due(&done_tx);
            let wake = self.next_due().unwrap_or_else(|| Instant::now() + Duration::from_secs(60));
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(Command::Reload { run, done }) = cmd else { return; };
                    self.load().await;
                    self.redefine();
                    self.publish();
                    match run.filter(|id| self.runnable(id)) {
                        Some(id) => {
                            if let Some(l) = self.live.get_mut(&id) {
                                l.next = Instant::now();
                            }
                            self.waiting.entry(id).or_default().push(done);
                        }
                        None => {
                            let _ = done.send(());
                        }
                    }
                }
                Some(result) = done_rx.recv() => {
                    let waiting = self.waiting.remove(&result.id).unwrap_or_default();
                    self.apply(result);
                    self.publish();
                    for w in waiting {
                        let _ = w.send(());
                    }
                }
                changed = self.containers.changed() => {
                    if changed.is_err() { return; }
                    if self.redefine() {
                        self.publish();
                    }
                }
                _ = tokio::time::sleep_until(wake) => {}
                _ = flush.tick() => {
                    flushes += 1;
                    self.flush(flushes.is_multiple_of(12)).await;
                    self.publish();
                }
            }
        }
    }

    async fn load(&mut self) {
        match self.store.call(|c| Ok((db::list(c)?, db::services(c)?))).await {
            Ok((custom, settings)) => {
                self.custom = custom;
                self.settings = settings;
            }
            Err(e) => tracing::error!(error = %e, "cannot read uptime checks"),
        }
    }

    /// Rebuilds the check list from the containers and settings. `true` if it
    /// changed.
    fn redefine(&mut self) -> bool {
        let snapshot = self.containers.borrow().clone();
        let now = unix_now();
        let fresh = define(&snapshot.containers, &self.settings, &self.custom, self.default_interval);
        let checks = carry_vanished(&self.checks, fresh, &mut self.vanished, now);
        if checks == self.checks {
            return false;
        }
        let open = self.events.as_ref().map(|e| e.open_problems()).unwrap_or_default();

        for old in &self.checks {
            let still = checks.iter().find(|c| c.check.id == old.check.id);
            let stopped = match still {
                None => true,
                Some(new) => !new.check.enabled || new.check.target != old.check.target,
            };
            if stopped {
                if let (Some(events), Some(r)) = (&self.events, detector::gone(&old.check.id, &old.check.name, &open)) {
                    events.report(r);
                }
            }
        }
        self.live.retain(|id, _| checks.iter().any(|c| &c.check.id == id));
        // A removed check's unsaved results would be written after its
        // history was deleted.
        self.stats.pending.retain(|(id, _), _| checks.iter().any(|c| &c.check.id == id));
        for c in &checks {
            let live = self.live.entry(c.check.id.clone()).or_insert_with(|| Live::new(&c.check.id, c.check.interval_secs));
            let before = self.checks.iter().find(|o| o.check.id == c.check.id);
            if before.is_some_and(|b| b.check.target != c.check.target || b.check.kind != c.check.kind) {
                // A new target starts a new history.
                *live = Live::new(&c.check.id, c.check.interval_secs);
            }
            if !(c.check.enabled && c.active) {
                live.streak = Streak::default();
                live.set_state(CheckState::Paused, now);
            } else if live.state == CheckState::Paused {
                live.set_state(CheckState::Pending, now);
            }
        }
        self.checks = checks;
        true
    }

    fn runnable(&self, id: &str) -> bool {
        self.checks.iter().any(|c| c.check.id == id && c.check.enabled && c.active)
    }

    fn next_due(&self) -> Option<Instant> {
        self.checks
            .iter()
            .filter(|c| c.check.enabled && c.active)
            .filter_map(|c| self.live.get(&c.check.id))
            .filter(|l| !l.running)
            .map(|l| l.next)
            .min()
    }

    fn start_due(&mut self, done: &mpsc::Sender<ProbeDone>) {
        let now = Instant::now();
        for c in self.checks.iter().filter(|c| c.check.enabled && c.active) {
            let Some(live) = self.live.get_mut(&c.check.id) else { continue };
            if live.running || live.next > now {
                continue;
            }
            live.running = true;
            live.next = now + Duration::from_secs(u64::from(c.check.interval_secs));
            let (prober, permits, done) = (self.prober.clone(), self.permits.clone(), done.clone());
            let check = c.check.clone();
            tokio::spawn(async move {
                let _permit = permits.acquire_owned().await;
                let at = unix_now();
                let outcome = prober.run(check.kind, &check.target, check.any_status).await;
                let _ = done.send(ProbeDone { id: check.id, target: check.target, at, outcome }).await;
            });
        }
    }

    fn apply(&mut self, r: ProbeDone) {
        let Some(def) = self.checks.iter().find(|c| c.check.id == r.id) else { return };
        let Some(live) = self.live.get_mut(&r.id) else { return };
        live.running = false;
        if def.check.target != r.target || !(def.check.enabled && def.active) {
            return;
        }

        let o = r.outcome;
        if live.recent.len() == RECENT {
            live.recent.pop_front();
        }
        live.recent.push_back(UptimeBeat { at: r.at, ok: o.ok, latency_ms: o.latency_ms, detail: o.detail.clone() });
        self.stats.record(&r.id, r.at, o.ok, o.latency_ms);

        let open = self.events.as_ref().map(|e| e.open_problems()).unwrap_or_default();
        let subject = Subject {
            id: &def.check.id,
            name: &def.check.name,
            target: &def.check.target,
            service: def.check.service.as_deref(),
            via_peer: def.check.via_peer.as_deref(),
        };
        if !o.ok && def.grace_until.is_some_and(|g| r.at < g) {
            if live.state != CheckState::Down {
                live.set_state(CheckState::Pending, r.at);
            }
            return;
        }
        let (state, report) = detector::observe(&mut live.streak, &subject, o.ok, o.detail.as_deref(), &open, r.at);
        live.set_state(state, r.at);

        if let Some(v) = o.cert {
            live.cert = Some(v.clone());
            if let Some(host) = reqwest::Url::parse(&def.check.target).ok().and_then(|u| u.host_str().map(str::to_string)) {
                self.certs.insert(host, v);
            }
        }
        if let Some(events) = &self.events {
            events.report_all(report);
            events.report_all(detector::certs(&self.certs, &open, r.at));
        }
    }

    async fn flush(&mut self, prune: bool) {
        let rows: Vec<(String, i64, Tally)> = self.stats.pending
            .drain()
            .map(|((id, hour), t)| (id, hour, t))
            .collect();
        if !rows.is_empty() {
            let retry = rows.clone();
            if let Err(e) = self.store.call(move |c| db::add_hourly(c, &rows)).await {
                tracing::warn!(error = %e, "cannot save uptime results");
                // Keep them for the next flush.
                for (id, hour, t) in retry {
                    self.stats.pending.entry((id, hour)).or_default().merge(&t);
                }
                return;
            }
        }
        if prune {
            let before = hour_start(unix_now() - RETAIN_SECS);
            if let Err(e) = self.store.call(move |c| db::prune(c, before)).await {
                tracing::warn!(error = %e, "cannot prune uptime results");
            }
        }
        self.refresh_stats().await;
    }

    async fn refresh_stats(&mut self) {
        let now = unix_now();
        let since = |secs: i64| hour_start(now - secs);
        let (d, m, q) = (since(86_400), since(30 * 86_400), since(90 * 86_400));
        match self.store.call(move |c| Ok((db::sums(c, d)?, db::sums(c, m)?, db::sums(c, q)?))).await {
            Ok((day, month, quarter)) => {
                self.stats.day = day;
                self.stats.month = month;
                self.stats.quarter = quarter;
            }
            Err(e) => tracing::warn!(error = %e, "cannot read uptime history"),
        }
    }

    fn publish(&self) {
        let now = unix_now();
        let entries: Vec<UptimeEntry> = self.checks
            .iter()
            .map(|c| {
                let live = self.live.get(&c.check.id);
                UptimeEntry {
                    check: c.check.clone(),
                    state: live.map_or(CheckState::Pending, |l| l.state),
                    since: live.and_then(|l| l.since),
                    recent: live.map(|l| l.recent.iter().cloned().collect()).unwrap_or_default(),
                    stats: self.stats.for_check(&c.check.id, now),
                    cert: live
                        .and_then(|l| l.cert.as_ref())
                        .map(|v| CertInfo { not_before: v.not_before, not_after: v.not_after }),
                }
            })
            .collect();
        let response = UptimeResponse { checks: entries, sampled_at: now };
        let json: Arc<str> = serde_json
            ::to_string(&response)
            .unwrap_or_else(|_| "{}".into())
            .into();
        let _ = self.snap_tx.send(Arc::new(UptimeSnapshot { json, entries: response.checks }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(name: &str, service: Option<&str>, url: Option<&str>, state: &str) -> ContainerInfo {
        ContainerInfo {
            id: name.into(),
            name: name.into(),
            image: "x".into(),
            status: String::new(),
            state: state.into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: None,
            compose_service: None,
            update_labels: Default::default(),
            helion_service: service.map(Into::into),
            helion_service_description: None,
            helion_service_url: url.map(Into::into),
            icon: None,
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
        }
    }

    #[test]
    fn services_with_a_url_get_a_check() {
        let containers = [
            ContainerInfo {
                started_at: Some("1970-01-01T00:16:40Z".into()),
                ..container("immich-server", Some("immich"), Some("https://immich.example.net"), "running")
            },
            container("immich-ml", Some("immich"), None, "exited"),
            container("portainer", Some("portainer"), Some("portainer.example.net"), "exited"),
            container("caddy", Some("system"), Some("x.example.net"), "running"),
            container("redis", None, None, "running"),
            container("nextcloud-cron", Some("nextcloud"), None, "running"),
        ];
        let settings = HashMap::from([
            ("immich".to_string(), UptimeServiceInput { enabled: true, path: Some("/api/ping".into()), any_status: true }),
        ]);
        let custom = [CustomCheck {
            id: 7,
            name: "router".into(),
            kind: CheckKind::Tcp,
            target: "192.168.1.1:443".into(),
            interval_secs: None,
            enabled: true,
            any_status: false,
            via_peer: None,
        }];
        let checks = define(&containers, &settings, &custom, 60);
        let ids: Vec<&str> = checks.iter().map(|c| c.check.id.as_str()).collect();
        assert_eq!(ids, ["svc:immich", "svc:portainer", "7"]);

        let immich = &checks[0];
        assert_eq!(immich.check.target, "https://immich.example.net/api/ping");
        assert!(immich.active && immich.check.any_status);
        let portainer = &checks[1];
        assert_eq!(portainer.check.target, "https://portainer.example.net/");
        assert!(!portainer.active, "nothing running, so nothing to check");
        assert_eq!(checks[2].check.interval_secs, 60);
        assert_eq!(immich.grace_until, Some(1_000 + STARTUP_GRACE));
        assert_eq!(portainer.grace_until, None);
    }

    #[test]
    fn a_service_recreated_keeps_its_check_and_one_removed_loses_it() {
        let running = [container("immich-server", Some("immich"), Some("immich.example.net"), "running")];
        let mut vanished = HashMap::new();
        let before = carry_vanished(&[], define(&running, &HashMap::new(), &[], 60), &mut vanished, 0);
        assert_eq!(before.len(), 1);

        // Mid-recreate: no container at all. The check stays, paused.
        let during = carry_vanished(&before, define(&[], &HashMap::new(), &[], 60), &mut vanished, 10);
        assert_eq!(during.len(), 1);
        assert!(!during[0].active);
        let during = carry_vanished(&during, define(&[], &HashMap::new(), &[], 60), &mut vanished, 30);
        assert_eq!(during.len(), 1);

        // Back: the new container's check, and the clock is forgotten.
        let after = carry_vanished(&during, define(&running, &HashMap::new(), &[], 60), &mut vanished, 40);
        assert!(after[0].active);
        assert!(vanished.is_empty());

        // Gone for good: dropped once the grace is over.
        let gone = carry_vanished(&after, define(&[], &HashMap::new(), &[], 60), &mut vanished, 100);
        let gone = carry_vanished(&gone, define(&[], &HashMap::new(), &[], 60), &mut vanished, 100 + VANISH_GRACE);
        assert!(gone.is_empty());
        assert!(vanished.is_empty());
    }

    #[test]
    fn validates_custom_checks() {
        let input = |kind, target: &str| UptimeCheckInput {
            name: " Router ".into(),
            kind,
            target: target.into(),
            interval_secs: Some(60),
            enabled: true,
            any_status: false,
            via_peer: None,
        };
        let ok = validate(input(CheckKind::Tcp, "192.168.1.1:443"), 0, 60, &[]).unwrap();
        assert_eq!(ok.name, "Router");
        assert_eq!(ok.interval_secs, None, "the default is stored as the default");
        assert!(validate(input(CheckKind::Tcp, "192.168.1.1"), 0, 60, &[]).is_err());
        assert!(validate(input(CheckKind::Http, "ftp://x"), 0, 60, &[]).is_err());
        assert!(validate(input(CheckKind::Http, "https://example.com/health"), 0, 60, &[]).is_ok());
        assert!(validate(UptimeCheckInput { interval_secs: Some(5), ..input(CheckKind::Tcp, "a:1") }, 0, 60, &[]).is_err());

        let path = |p: &str| validate_service(UptimeServiceInput { enabled: true, path: Some(p.into()), any_status: false });
        assert_eq!(path(" / ").unwrap().path, None);
        assert_eq!(path("/api/ping").unwrap().path.as_deref(), Some("/api/ping"));
        assert!(path("api").is_err());
    }

    #[test]
    fn stats_include_results_not_yet_written() {
        let mut stats = Stats::default();
        stats.day.insert("a".into(), Tally { ok: 2, fail: 0, latency_sum: 20, latency_max: 10 });
        stats.record("a", 10_000, false, None);
        stats.record("a", 10_000, true, Some(40));
        let s = stats.for_check("a", 10_000);
        assert_eq!(s.day, Some(0.75));
        assert_eq!(s.latency_ms, Some(20));
        assert_eq!(stats.for_check("b", 10_000), UptimeStats::default());
    }

    #[tokio::test]
    async fn a_new_check_answers_with_its_first_result() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let store = Store::in_memory();
        let saved = store.with(|c| {
            db::insert(c, &CustomCheck {
                id: 0,
                name: "local".into(),
                kind: CheckKind::Tcp,
                target: format!("127.0.0.1:{port}"),
                interval_secs: Some(3_600),
                enabled: true,
                any_status: false,
                via_peer: None,
            }).unwrap()
        });
        let (_tx, containers) = watch::channel(
            Arc::new(ContainerSnapshot { json: "[]".into(), running: Arc::new([]), containers: Arc::new([]), sampled_at: 0 })
        );
        let cfg = UptimeConfig { enabled: true, interval_secs: 60, local_domains: vec![] };
        let handle = spawn(&cfg, store, containers, None, Vec::new());

        let id = saved.id.to_string();
        handle.reload(Some(id.clone())).await.unwrap();
        let entry = handle.entry(&id).unwrap();
        assert_eq!(entry.state, CheckState::Up);
        assert_eq!(entry.recent.len(), 1);
        assert_eq!(entry.stats.day, Some(1.0));
        drop(listener);
    }
}
