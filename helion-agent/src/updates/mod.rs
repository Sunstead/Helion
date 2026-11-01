//! Image updates: newer tags for what's running, applied through the
//! infrastructure repository so git stays the record of what runs.
//!
//! Every few hours the agent lists each image repository's tags (anonymously;
//! `registry.rs`) and works out, per unit, what the labels allow
//! (`rules.rs`). Applying one never touches compose here: the agent asks
//! GitHub to run the repository's update workflow (`github.rs`), which checks
//! the request itself, backs up first where there's a database, commits the
//! new tag and deploys. The agent then follows the run and watches the
//! service for a while (`decide.rs`): a check that goes down or a container
//! that keeps restarting marks the update broken, pauses automatic updates
//! for that unit and says so. Nothing is rolled back on its own.
//!
//! One task owns all of this, like the other UI-edited features: handlers
//! send it a command and read the snapshot it publishes. Runs are stored as
//! they move, so one in flight survives the deploy restarting this agent.

pub mod db;
pub mod decide;
pub mod github;
pub mod image;
pub mod registry;
pub mod rules;
pub mod tags;

use crate::{
    backups::BackupSnapshot,
    config::UpdatesConfig,
    docker::DockerHandle,
    error::AgentError,
    events::{ EventsHandle, NewEvent, ProblemSpec, Report, Resolution },
    sample::{ docker::ContainerSnapshot, host::unix_now },
    store::Store,
    uptime::UptimeHandle,
};
use helion_common::types::{
    EventCategory,
    Severity,
    UpdatePolicy,
    UpdateRun,
    UpdateRunKind,
    UpdateRunState,
    UpdateUnit,
    UpdatesResponse,
};
use db::StoredRun;
use decide::Verdict;
use github::GitHub;
use registry::Registry;
use rules::{ Offer, Unit };
use std::{ collections::HashMap, sync::Arc, time::Duration };
use tokio::{ sync::{ mpsc, oneshot, watch }, time::Instant };

/// How often a run in flight is followed.
const FOLLOW: Duration = Duration::from_secs(15);
/// A dispatched workflow that GitHub never shows as a run.
const NEVER_STARTED: i64 = 300;
/// Warn this long before the GitHub token expires.
const TOKEN_WARNING: i64 = 14 * 86_400;
const HISTORY: usize = 30;
const TOKEN_KEY: &str = "update:token";

pub struct UpdatesSnapshot {
    pub json: Arc<str>,
}

enum Command {
    Check(oneshot::Sender<()>),
    Apply {
        unit: String,
        tag: Option<String>,
        kind: UpdateRunKind,
        by: String,
        done: oneshot::Sender<Result<UpdateRun, AgentError>>,
    },
    Policy {
        unit: String,
        policy: UpdatePolicy,
        done: oneshot::Sender<Result<(), AgentError>>,
    },
}

#[derive(Clone)]
pub struct UpdatesHandle {
    pub rx: watch::Receiver<Arc<UpdatesSnapshot>>,
    /// A token and a repository are set, so updates can be applied.
    pub can_apply: bool,
    tx: mpsc::Sender<Command>,
}

fn stopped() -> AgentError {
    AgentError::Unavailable("the update task stopped".into())
}

impl UpdatesHandle {
    /// Checks the registries now, and waits for the new snapshot.
    pub async fn check(&self) -> Result<(), AgentError> {
        let (done, wait) = oneshot::channel();
        self.tx.send(Command::Check(done)).await.map_err(|_| stopped())?;
        wait.await.map_err(|_| stopped())
    }

    /// Queues an update to `tag`, or a rollback to the tag before the last
    /// update when `kind` is a rollback.
    pub async fn apply(&self, unit: String, tag: Option<String>, kind: UpdateRunKind, by: String) -> Result<UpdateRun, AgentError> {
        let (done, wait) = oneshot::channel();
        self.tx.send(Command::Apply { unit, tag, kind, by, done }).await.map_err(|_| stopped())?;
        wait.await.map_err(|_| stopped())?
    }

    pub async fn policy(&self, unit: String, policy: UpdatePolicy) -> Result<(), AgentError> {
        let (done, wait) = oneshot::channel();
        self.tx.send(Command::Policy { unit, policy, done }).await.map_err(|_| stopped())?;
        wait.await.map_err(|_| stopped())?
    }
}

pub struct Inputs {
    pub store: Store,
    pub docker: DockerHandle,
    pub containers: watch::Receiver<Arc<ContainerSnapshot>>,
    pub backups: Option<watch::Receiver<Arc<BackupSnapshot>>>,
    pub uptime: Option<UptimeHandle>,
    pub events: Option<EventsHandle>,
}

pub fn spawn(cfg: &UpdatesConfig, inputs: Inputs) -> UpdatesHandle {
    let client = reqwest::Client
        ::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("helion-agent/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client");
    let github = match (&cfg.repo, &cfg.token) {
        (Some(repo), Some(token)) => Some(GitHub::new(client.clone(), token.clone(), repo, &cfg.workflow, &cfg.git_ref)),
        _ => None,
    };
    let (snap_tx, rx) = watch::channel(Arc::new(UpdatesSnapshot { json: "{\"units\":[],\"checked_at\":null,\"errors\":[],\"can_apply\":false,\"token_expires_at\":null,\"history\":[],\"min_age_days\":3}".into() }));
    let (tx, cmd_rx) = mpsc::channel(16);
    let handle = UpdatesHandle { rx, can_apply: github.is_some(), tx };
    let task = Task {
        cfg: cfg.clone(),
        store: inputs.store,
        docker: inputs.docker,
        containers: inputs.containers,
        backups: inputs.backups,
        uptime: inputs.uptime,
        events: inputs.events,
        github,
        registry: Registry::new(client),
        units: Vec::new(),
        tags: HashMap::new(),
        seen: HashMap::new(),
        notes: HashMap::new(),
        policies: HashMap::new(),
        runs: Vec::new(),
        checked_at: None,
        errors: Vec::new(),
        last_backup: None,
        snap_tx,
    };
    tokio::spawn(task.run(cmd_rx));
    handle
}

struct Task {
    cfg: UpdatesConfig,
    store: Store,
    docker: DockerHandle,
    containers: watch::Receiver<Arc<ContainerSnapshot>>,
    backups: Option<watch::Receiver<Arc<BackupSnapshot>>>,
    uptime: Option<UptimeHandle>,
    events: Option<EventsHandle>,
    github: Option<GitHub>,
    registry: Registry,
    units: Vec<Unit>,
    /// Newer tags per repository, from the last check.
    tags: HashMap<String, Vec<String>>,
    seen: HashMap<(String, String), i64>,
    /// Release notes per repository, from the image's source label.
    notes: HashMap<String, Option<String>>,
    policies: HashMap<String, (UpdatePolicy, Option<String>)>,
    /// Newest first.
    runs: Vec<StoredRun>,
    checked_at: Option<i64>,
    errors: Vec<String>,
    /// The backup status last seen, to notice a new nightly.
    last_backup: Option<String>,
    snap_tx: watch::Sender<Arc<UpdatesSnapshot>>,
}

fn run_id() -> String {
    let nanos = std::time::SystemTime
        ::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{:06x}", nanos / 1_000_000_000, (nanos / 1_000) % 0x1000000)
}

/// Waits for the next backup status: `false` once there will be none, and
/// never without backups. It waits on the receiver itself: a clone made for
/// each wait starts from the version the original last saw, and marking the
/// clone seen leaves the original behind, so every later clone reports the
/// same change at once and the loop spins (0.8 and 0.9.0 held a core that way
/// from the first status change on).
async fn backup_changed<T>(rx: &mut Option<watch::Receiver<T>>) -> bool {
    match rx {
        Some(rx) => rx.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

impl Task {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<Command>) {
        match self.store.call(|c| Ok((db::policies(c)?, db::recent_runs(c, HISTORY)?, db::open_runs(c)?))).await {
            Ok((policies, mut runs, open)) => {
                // A run the agent was following when it stopped (the deploy
                // restarts it) carries on, however old.
                for r in open {
                    if !runs.iter().any(|x| x.run.id == r.run.id) {
                        runs.push(r);
                    }
                }
                if runs.iter().any(|r| !r.run.state.finished()) {
                    tracing::info!("resuming an update in flight");
                }
                self.policies = policies;
                self.runs = runs;
            }
            Err(e) => tracing::error!(error = %e, "cannot read update state"),
        }
        self.last_backup = self.backups.as_ref().map(|b| b.borrow().status.generated_at.clone());
        self.refresh_units();
        self.publish();

        // Soon after start (the containers are listed by then), then on the
        // interval.
        let mut next_check = Instant::now() + Duration::from_secs(90);
        let every = Duration::from_secs(u64::from(self.cfg.check_interval_hours) * 3_600);
        // A deadline, not a sleep made fresh each time round: the container
        // list changes every 2 s, and a new 15 s sleep would never finish.
        let mut next_follow = Instant::now() + FOLLOW;

        loop {
            let active = self.runs.iter().any(|r| !r.run.state.finished());
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return; };
                    self.command(cmd).await;
                }
                changed = self.containers.changed() => {
                    if changed.is_err() { return; }
                    if self.refresh_units() {
                        self.publish();
                    }
                }
                changed = backup_changed(&mut self.backups) => {
                    if !changed {
                        self.backups = None;
                        continue;
                    }
                    self.after_backup().await;
                }
                _ = tokio::time::sleep_until(next_check) => {
                    self.check().await;
                    next_check = Instant::now() + every;
                    self.publish();
                }
                _ = tokio::time::sleep_until(next_follow), if active => {
                    self.drive().await;
                    self.publish();
                    next_follow = Instant::now() + FOLLOW;
                }
            }
        }
    }

    async fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Check(done) => {
                self.check().await;
                self.publish();
                let _ = done.send(());
            }
            Command::Apply { unit, tag, kind, by, done } => {
                let result = self.queue(&unit, tag, kind, &by).await;
                if result.is_ok() {
                    self.drive().await;
                }
                self.publish();
                let _ = done.send(result);
            }
            Command::Policy { unit, policy, done } => {
                let result = if self.units.iter().any(|u| u.id == unit) {
                    let key = unit.clone();
                    self.store.call(move |c| db::set_policy(c, &key, policy)).await.map(|_| {
                        self.policies.insert(unit, (policy, None));
                    })
                } else {
                    Err(AgentError::NotFound(format!("no update unit {unit}")))
                };
                self.publish();
                let _ = done.send(result);
            }
        }
    }

    /// Rebuilds the units from the containers. `true` if they changed.
    fn refresh_units(&mut self) -> bool {
        let units = rules::units(&self.containers.borrow().containers);
        if units == self.units {
            return false;
        }
        self.units = units;
        true
    }

    fn offer(&self, unit: &Unit) -> Offer {
        rules::offer(unit, &self.tags, &self.seen)
    }

    async fn check(&mut self) {
        self.refresh_units();
        let mut errors = Vec::new();
        let now = unix_now();
        // Each repository once, against the tag its unit runs.
        let mut repos: Vec<(image::ImageRef, String)> = Vec::new();
        for u in self.units.iter().filter(|u| !u.rules.off) {
            for i in u.images() {
                if !repos.iter().any(|(r, _)| r.repo == i.repo) {
                    repos.push((i.clone(), u.current.clone()));
                }
            }
        }
        for (image, current) in repos {
            match self.registry.tags(&image).await {
                Ok(all) => {
                    let base = tags::Version::parse(&current);
                    let newer: Vec<String> = all
                        .into_iter()
                        .filter(|t| {
                            match (&base, tags::Version::parse(t)) {
                                (Some(b), Some(v)) => b.same_shape(&v) && v.nums > b.nums,
                                _ => false,
                            }
                        })
                        .collect();
                    let (repo, list) = (image.repo.clone(), newer.clone());
                    match self.store.call(move |c| db::see(c, &repo, &list, now)).await {
                        Ok(seen) => {
                            for (tag, at) in seen {
                                self.seen.insert((image.repo.clone(), tag), at);
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "cannot record seen tags"),
                    }
                    self.tags.insert(image.repo.clone(), newer);
                }
                Err(e) => errors.push(e),
            }
            if !self.notes.contains_key(&image.repo) {
                let notes = self.release_notes(&format!("{}:{}", image.repo, image.tag)).await;
                self.notes.insert(image.repo.clone(), notes);
            }
        }
        if let Some(gh) = &self.github {
            // Proves the token works, and learns when it expires.
            if let Err(e) = gh.find_run("-").await {
                errors.push(e);
            }
            self.token_expiry(now);
        }
        self.checked_at = Some(now);
        if !errors.is_empty() {
            tracing::warn!(errors = ?errors, "update check had errors");
        }
        self.errors = errors;
    }

    /// The image's `org.opencontainers.image.source`, as a releases page
    /// when it's on GitHub.
    async fn release_notes(&self, image: &str) -> Option<String> {
        let client = self.docker.client()?;
        let inspect = client.inspect_image(image).await.ok()?;
        let source = inspect.config?.labels?.remove("org.opencontainers.image.source")?;
        let source = source.trim().trim_end_matches(".git").trim_end_matches('/').to_string();
        if !source.starts_with("https://") {
            return None;
        }
        Some(if source.starts_with("https://github.com/") { format!("{source}/releases") } else { source })
    }

    fn token_expiry(&self, now: i64) {
        let (Some(events), Some(gh)) = (&self.events, &self.github) else { return };
        let Some(expires) = gh.expires_at() else { return };
        if expires - now < TOKEN_WARNING {
            let days = ((expires - now).max(0) / 86_400) as u64;
            events.report(
                Report::Open(ProblemSpec {
                    key: TOKEN_KEY.into(),
                    category: EventCategory::Update,
                    kind: "token_expiring",
                    severity: Severity::Warning,
                    subject: "GitHub".into(),
                    title: if days == 0 { "The GitHub token for updates has expired".into() } else { format!("The GitHub token for updates expires in {days} days") },
                    detail: Some("Make a new one with the same permissions and put it in the server's .env.".into()),
                    service: None,
                    depends_on: None,
                })
            );
        } else {
            events.report(Report::Resolve(Resolution { key: TOKEN_KEY.into(), title: "The GitHub token for updates was renewed".into(), detail: None }));
        }
    }

    async fn queue(&mut self, unit_id: &str, tag: Option<String>, kind: UpdateRunKind, by: &str) -> Result<UpdateRun, AgentError> {
        if self.github.is_none() {
            return Err(AgentError::NotEnabled("applying updates"));
        }
        let unit = self.units
            .iter()
            .find(|u| u.id == unit_id)
            .cloned()
            .ok_or_else(|| AgentError::NotFound(format!("no update unit {unit_id}")))?;
        if let Some(b) = &unit.blocked {
            return Err(AgentError::BadRequest(format!("{} can't be updated: {b}", unit.name)));
        }
        if self.runs.iter().any(|r| r.run.unit == unit.id && !r.run.state.finished()) {
            return Err(AgentError::BadRequest(format!("{} is already being updated", unit.name)));
        }
        let to = match kind {
            UpdateRunKind::Update => {
                let tag = tag.ok_or_else(|| AgentError::BadRequest("which tag?".into()))?;
                let offer = self.offer(&unit);
                let allowed = [offer.available.as_ref(), offer.patch.as_ref()].into_iter().flatten().any(|c| c.tag == tag);
                if !allowed {
                    return Err(AgentError::BadRequest(format!("{tag} isn't an update the rules allow for {}", unit.name)));
                }
                tag
            }
            UpdateRunKind::Rollback => {
                let key = unit.id.clone();
                self.store
                    .call(move |c| db::previous(c, &key)).await?
                    .filter(|p| *p != unit.current)
                    .ok_or_else(|| AgentError::BadRequest(format!("there's no earlier version of {} to go back to", unit.name)))?
            }
        };
        let stored = StoredRun {
            run: UpdateRun {
                id: run_id(),
                unit: unit.id.clone(),
                kind,
                from: unit.current.clone(),
                to,
                by: by.to_string(),
                state: UpdateRunState::Queued,
                requested_at: unix_now(),
                finished_at: None,
                run_url: None,
                detail: None,
                step: None,
                watch_until: None,
            },
            services: unit.services(),
            run_id: None,
            watch_until: None,
        };
        self.save(stored.clone()).await;
        Ok(stored.run)
    }

    /// Stores a run and keeps it in the list, newest first.
    async fn save(&mut self, run: StoredRun) {
        let copy = run.clone();
        if let Err(e) = self.store.call(move |c| db::save_run(c, &copy)).await {
            tracing::warn!(error = %e, "cannot save update run");
        }
        match self.runs.iter_mut().find(|r| r.run.id == run.run.id) {
            Some(existing) => *existing = run,
            None => {
                self.runs.insert(0, run);
                self.runs.truncate(HISTORY);
            }
        }
    }

    /// Moves every run along. Runs being watched are judged each time. One
    /// workflow runs at a time (a second would only wait in the workflow's
    /// concurrency group), but the next queued run starts as soon as the one
    /// before has deployed, without waiting out its watch.
    async fn drive(&mut self) {
        let Some(gh) = self.github.clone() else { return };
        let watching: Vec<StoredRun> = self.runs
            .iter()
            .filter(|r| r.run.state == UpdateRunState::Watching)
            .cloned()
            .collect();
        for run in watching {
            self.watch(run).await;
        }
        // Twice at most: a run that has just deployed makes way for the next.
        for _ in 0..2 {
            let active = self.runs
                .iter()
                .find(|r| matches!(r.run.state, UpdateRunState::Dispatched | UpdateRunState::Running))
                .cloned();
            let run = match active {
                Some(r) => r,
                None => {
                    let Some(next) = self.runs.iter().rev().find(|r| r.run.state == UpdateRunState::Queued).cloned() else {
                        return;
                    };
                    next
                }
            };
            let queued = run.run.state == UpdateRunState::Queued;
            let id = run.run.id.clone();
            self.advance(&gh, run).await;
            let still_active = self.runs
                .iter()
                .any(|r| r.run.id == id && matches!(r.run.state, UpdateRunState::Dispatched | UpdateRunState::Running));
            if queued || still_active {
                return;
            }
        }
    }

    /// One step for the run with the workflow: start it, find it, or follow it.
    async fn advance(&mut self, gh: &GitHub, mut run: StoredRun) {
        let now = unix_now();
        match run.run.state {
            UpdateRunState::Queued => {
                let inputs = serde_json::json!({
                    "request_id": run.run.id,
                    "services": run.services.join(","),
                    "from": run.run.from,
                    "to": run.run.to,
                    "kind": run.run.kind.as_str(),
                    "by": run.run.by,
                });
                match gh.dispatch(&inputs).await {
                    Ok(Some(info)) => {
                        run.run.state = UpdateRunState::Running;
                        run.run_id = Some(info.id);
                        run.run.run_url = Some(info.html_url).filter(|u| !u.is_empty());
                    }
                    Ok(None) => run.run.state = UpdateRunState::Dispatched,
                    Err(e) => {
                        self.finish(run, UpdateRunState::Failed, Some(e)).await;
                        return;
                    }
                }
            }
            UpdateRunState::Dispatched => {
                match gh.find_run(&run.run.id).await {
                    Ok(Some(info)) => {
                        run.run.state = UpdateRunState::Running;
                        run.run_id = Some(info.id);
                        run.run.run_url = Some(info.html_url).filter(|u| !u.is_empty());
                    }
                    Ok(None) if now - run.run.requested_at > NEVER_STARTED => {
                        self.finish(run, UpdateRunState::Failed, Some("GitHub never started the update workflow".into())).await;
                        return;
                    }
                    Ok(None) => {
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot find the update run");
                        return;
                    }
                }
            }
            UpdateRunState::Running => {
                let Some(id) = run.run_id else { return };
                match gh.run(id).await {
                    Ok(info) if info.status == "completed" => {
                        match info.conclusion.as_deref() {
                            Some("success") => {
                                run.run.state = UpdateRunState::Watching;
                                run.run.step = None;
                                run.watch_until = Some(now + i64::from(self.cfg.watch_minutes) * 60);
                            }
                            other => {
                                let why = match other {
                                    Some("cancelled") => "the workflow was cancelled".to_string(),
                                    Some(c) => format!("the workflow ended with {c}; its log says why"),
                                    None => "the workflow ended without a result".to_string(),
                                };
                                self.finish(run, UpdateRunState::Failed, Some(why)).await;
                                return;
                            }
                        }
                    }
                    Ok(info) => {
                        // Between steps, the last one named stays up.
                        let step = if info.status == "in_progress" {
                            match gh.step(id).await {
                                Ok(step) => step.or_else(|| run.run.step.clone()),
                                Err(e) => {
                                    tracing::warn!(error = %e, "cannot read the update run's steps");
                                    run.run.step.clone()
                                }
                            }
                        } else {
                            Some("Waiting for another deploy".to_string())
                        };
                        if step == run.run.step {
                            return;
                        }
                        run.run.step = step;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot follow the update run");
                        return;
                    }
                }
            }
            _ => {
                return;
            }
        }
        self.save(run).await;
    }

    /// Judges a deployed run until its watch ends.
    async fn watch(&mut self, run: StoredRun) {
        let containers = self.containers.borrow().containers.clone();
        let uptime = self.uptime
            .as_ref()
            .map(|u| u.rx.borrow().entries.clone())
            .unwrap_or_default();
        match decide::judge(&run, &containers, &uptime, unix_now()) {
            Verdict::Wait => {}
            Verdict::Done => self.finish(run, UpdateRunState::Done, None).await,
            Verdict::Broken(why) => self.finish(run, UpdateRunState::Broken, Some(why)).await,
            Verdict::Failed(why) => self.finish(run, UpdateRunState::Failed, Some(why)).await,
        }
    }

    async fn finish(&mut self, mut run: StoredRun, state: UpdateRunState, detail: Option<String>) {
        run.run.state = state;
        run.run.finished_at = Some(unix_now());
        run.run.detail = detail.clone();
        let name = self.units
            .iter()
            .find(|u| u.id == run.run.unit)
            .map_or_else(|| run.run.unit.clone(), |u| u.name.clone());
        let (to, by) = (run.run.to.clone(), run.run.by.clone());
        let verb = if run.run.kind == UpdateRunKind::Rollback { "Rolled back" } else { "Updated" };
        let broken_key = format!("update:{}:broken", run.run.unit);
        tracing::info!(unit = %run.run.unit, to = %to, state = state.as_str(), detail = ?detail, "update finished");

        if state == UpdateRunState::Broken {
            let reason = format!("{to} broke it: {}", detail.clone().unwrap_or_default());
            let (unit, why) = (run.run.unit.clone(), reason.clone());
            if let Err(e) = self.store.call(move |c| db::pause(c, &unit, &why)).await {
                tracing::warn!(error = %e, "cannot pause automatic updates");
            }
            let policy = self.policies.get(&run.run.unit).map_or(UpdatePolicy::Manual, |p| p.0);
            self.policies.insert(run.run.unit.clone(), (policy, Some(reason)));
        }
        if let Some(events) = &self.events {
            let service = self.units
                .iter()
                .find(|u| u.id == run.run.unit)
                .and_then(|u| u.members.iter().find_map(|m| m.helion_service.clone()))
                .filter(|s| s != "system");
            match state {
                UpdateRunState::Done => {
                    let event = NewEvent::new(EventCategory::Update, "updated", Severity::Info, &name, format!("{verb} {name} to {to}"))
                        .actor(by)
                        .service(service.clone());
                    events.record(event);
                    events.report(
                        Report::Resolve(Resolution { key: broken_key, title: format!("{name} is healthy on {to}"), detail: None })
                    );
                }
                UpdateRunState::Failed => {
                    let event = NewEvent::new(EventCategory::Update, "update_failed", Severity::Warning, &name, format!("Couldn't update {name} to {to}"))
                        .actor(by)
                        .service(service);
                    events.record(match &detail {
                        Some(d) => event.detail(d.clone()),
                        None => event,
                    });
                }
                UpdateRunState::Broken => {
                    events.report(
                        Report::Open(ProblemSpec {
                            key: broken_key,
                            category: EventCategory::Update,
                            kind: "broken",
                            severity: Severity::Error,
                            subject: name.clone(),
                            title: format!("{name} {to} looks broken"),
                            detail: Some(
                                format!(
                                    "{}. Automatic updates for it are paused. Roll back from the Updates page.",
                                    detail.unwrap_or_default()
                                )
                            ),
                            service,
                            depends_on: None,
                        })
                    );
                }
                _ => {}
            }
        }
        self.save(run).await;
    }

    /// After a successful nightly backup, applies what the policies allow.
    /// That backup is the pre-update backup, so the workflow skips its own.
    async fn after_backup(&mut self) {
        let Some(status) = self.backups.as_ref().map(|b| b.borrow().status.clone()) else { return };
        if self.last_backup.as_deref() == Some(status.generated_at.as_str()) {
            return;
        }
        let first = self.last_backup.is_none();
        self.last_backup = Some(status.generated_at.clone());
        if first || self.github.is_none() || !decide::nightly_succeeded(&status) {
            return;
        }
        let now = unix_now();
        let min_age = i64::from(self.cfg.min_age_days) * 86_400;
        let targets: Vec<(String, String)> = self.units
            .iter()
            .filter_map(|u| {
                let (policy, paused) = self.policies.get(&u.id).cloned().unwrap_or_default();
                let tag = decide::auto_target(policy, paused.is_some(), &self.offer(u), min_age, now)?;
                Some((u.id.clone(), tag))
            })
            .collect();
        for (unit, tag) in targets {
            match self.queue(&unit, Some(tag.clone()), UpdateRunKind::Update, "auto").await {
                Ok(_) => tracing::info!(%unit, %tag, "queued an automatic update"),
                Err(e) => tracing::warn!(%unit, %tag, error = %e, "cannot queue an automatic update"),
            }
        }
        self.drive().await;
        self.publish();
    }

    fn publish(&self) {
        let units = self.units
            .iter()
            .map(|u| {
                let offer = self.offer(u);
                let (policy, paused) = self.policies.get(&u.id).cloned().unwrap_or_default();
                let run = self.runs
                    .iter()
                    .find(|r| r.run.unit == u.id)
                    .map(|r| UpdateRun { watch_until: r.watch_until, ..r.run.clone() });
                let previous = self.runs
                    .iter()
                    .find(|r| {
                        r.run.unit == u.id &&
                            r.run.kind == UpdateRunKind::Update &&
                            matches!(r.run.state, UpdateRunState::Done | UpdateRunState::Broken) &&
                            r.run.from != u.current
                    })
                    .map(|r| r.run.from.clone());
                UpdateUnit {
                    id: u.id.clone(),
                    name: u.name.clone(),
                    services: u.services(),
                    images: u.images().iter().map(|i| i.repo.clone()).collect(),
                    current: u.current.clone(),
                    available: offer.available,
                    patch: offer.patch,
                    held: offer.held,
                    notes_url: u.images().first().and_then(|i| self.notes.get(&i.repo).cloned().flatten()),
                    policy,
                    paused,
                    backup: u.rules.backup,
                    blocked: offer.blocked,
                    run,
                    previous,
                }
            })
            .collect();
        let response = UpdatesResponse {
            units,
            checked_at: self.checked_at,
            errors: self.errors.clone(),
            can_apply: self.github.is_some(),
            token_expires_at: self.github.as_ref().and_then(|g| g.expires_at()),
            history: self.runs.iter().map(|r| r.run.clone()).collect(),
            min_age_days: self.cfg.min_age_days,
        };
        let json: Arc<str> = serde_json
            ::to_string(&response)
            .unwrap_or_else(|_| "{}".into())
            .into();
        let _ = self.snap_tx.send(Arc::new(UpdatesSnapshot { json }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helion_common::types::ContainerInfo;
    use std::sync::atomic::{ AtomicBool, Ordering };
    use tokio::io::{ AsyncReadExt, AsyncWriteExt };

    /// A GitHub whose runs finish at once, successfully.
    async fn fake_github() -> String {
        fake_github_until(Arc::new(AtomicBool::new(true))).await
    }

    /// A GitHub whose runs are on "Pull the new images" until `finished`,
    /// then succeed.
    async fn fake_github_until(finished: Arc<AtomicBool>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("");
                let body = if req.starts_with("POST") {
                    r#"{"workflow_run_id":42,"html_url":"https://github.com/o/r/actions/runs/42"}"#
                } else if path.ends_with("/jobs") {
                    r#"{"jobs":[{"name":"bump","status":"in_progress","steps":[{"name":"Pull the new images","status":"in_progress"}]}]}"#
                } else if finished.load(Ordering::Relaxed) {
                    r#"{"id":42,"status":"completed","conclusion":"success","html_url":"https://github.com/o/r/actions/runs/42"}"#
                } else {
                    r#"{"id":42,"status":"in_progress","conclusion":null,"html_url":"https://github.com/o/r/actions/runs/42"}"#
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn ntfy(tag: &str) -> ContainerInfo {
        container("ntfy", "binwiederhier/ntfy", tag)
    }

    fn container(service: &str, repo: &str, tag: &str) -> ContainerInfo {
        ContainerInfo {
            id: service.into(),
            name: service.into(),
            image: format!("{repo}:{tag}"),
            status: String::new(),
            state: "running".into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: Some("jupiter".into()),
            compose_service: Some(service.into()),
            update_labels: Default::default(),
            helion_service: Some(service.into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
        }
    }

    fn snapshot(c: ContainerInfo) -> Arc<ContainerSnapshot> {
        snapshot_of(vec![c])
    }

    fn snapshot_of(c: Vec<ContainerInfo>) -> Arc<ContainerSnapshot> {
        Arc::new(ContainerSnapshot { json: "[]".into(), running: Arc::new([]), containers: c.into(), sampled_at: 0 })
    }

    fn task(api: &str, containers: watch::Receiver<Arc<ContainerSnapshot>>, watch_minutes: u32) -> (Task, watch::Receiver<Arc<UpdatesSnapshot>>) {
        let (snap_tx, rx) = watch::channel(Arc::new(UpdatesSnapshot { json: "{}".into() }));
        let client = reqwest::Client::new();
        let task = Task {
            cfg: UpdatesConfig { watch_minutes, ..UpdatesConfig::default() },
            store: Store::in_memory(),
            docker: DockerHandle::new("/nonexistent.sock".into()),
            containers,
            backups: None,
            uptime: None,
            events: None,
            github: Some(GitHub::with_api(client.clone(), api, github::Secret("t".into()), "o/r", "update.yml", "main")),
            registry: Registry::new(client),
            units: Vec::new(),
            tags: HashMap::new(),
            seen: HashMap::new(),
            notes: HashMap::new(),
            policies: HashMap::new(),
            runs: Vec::new(),
            checked_at: None,
            errors: Vec::new(),
            last_backup: None,
            snap_tx,
        };
        (task, rx)
    }

    /// The loop itself, with the container list changing every 2 s as it
    /// does in production: a run it resumes still gets followed.
    #[tokio::test(start_paused = true)]
    async fn follows_runs_while_the_containers_keep_changing() {
        let api = fake_github().await;
        let (containers_tx, containers) = watch::channel(snapshot(ntfy("v2.29.0")));
        let (task, rx) = task(&api, containers, 10);
        let running = StoredRun {
            run: UpdateRun {
                id: "r1".into(),
                unit: "binwiederhier/ntfy".into(),
                kind: UpdateRunKind::Update,
                from: "v2.28.0".into(),
                to: "v2.29.0".into(),
                by: "riley".into(),
                state: UpdateRunState::Running,
                requested_at: unix_now(),
                finished_at: None,
                run_url: None,
                detail: None,
                step: None,
                watch_until: None,
            },
            services: vec!["ntfy".into()],
            run_id: Some(42),
            watch_until: None,
        };
        task.store.with(|c| db::save_run(c, &running).unwrap());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if containers_tx.send(snapshot(ntfy("v2.29.0"))).is_err() {
                    return;
                }
            }
        });
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        tokio::spawn(task.run(cmd_rx));

        // Under a minute, so the first update check (at 90 s) doesn't run.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let published: serde_json::Value = serde_json::from_str(&rx.borrow().json).unwrap_or_default();
            if published["history"][0]["state"] == "watching" {
                return;
            }
        }
        panic!("the run was never followed: {}", rx.borrow().json);
    }

    #[tokio::test(start_paused = true)]
    async fn a_backup_status_wakes_the_loop_once() {
        let never = |rx| async move {
            let mut rx = rx;
            tokio::time::timeout(Duration::from_secs(60), backup_changed(&mut rx)).await.is_err()
        };
        let (tx, rx) = watch::channel(0u32);
        let mut rx = Some(rx);
        assert!(never(rx.clone()).await, "nothing sent yet");

        // The old loop waited on a fresh clone each pass: the original never
        // took the change, so every clone reported it again, at once.
        tx.send(1).unwrap();
        let stale = rx.clone().unwrap();
        for _ in 0..3 {
            assert!(backup_changed(&mut Some(stale.clone())).await, "the spin");
        }

        assert!(backup_changed(&mut rx).await);
        assert!(never(rx.clone()).await, "taken once, on the receiver itself");

        drop(tx);
        assert!(!backup_changed(&mut rx).await, "no more statuses");
        assert!(never(None::<watch::Receiver<u32>>).await, "no backups: never");
    }

    #[tokio::test]
    async fn the_next_update_starts_while_the_last_is_watched() {
        let finished = Arc::new(AtomicBool::new(false));
        let api = fake_github_until(finished.clone()).await;
        let (_containers_tx, containers) = watch::channel(
            snapshot_of(vec![ntfy("v2.28.0"), container("uptime-kuma", "louislam/uptime-kuma", "2.5.4")])
        );
        let (mut task, rx) = task(&api, containers, 10);
        task.tags = HashMap::from([
            ("binwiederhier/ntfy".to_string(), vec!["v2.29.0".to_string()]),
            ("louislam/uptime-kuma".to_string(), vec!["2.5.5".to_string()]),
        ]);
        task.refresh_units();

        let a = task.queue("binwiederhier/ntfy", Some("v2.29.0".into()), UpdateRunKind::Update, "riley").await.unwrap();
        task.drive().await;
        task.drive().await;
        let run = |task: &Task, id: &str| task.runs.iter().find(|r| r.run.id == id).unwrap().run.clone();
        assert_eq!(run(&task, &a.id).state, UpdateRunState::Running);
        assert_eq!(run(&task, &a.id).step.as_deref(), Some("Pull the new images"), "the step under way");

        let b = task.queue("louislam/uptime-kuma", Some("2.5.5".into()), UpdateRunKind::Update, "riley").await.unwrap();
        task.drive().await;
        assert_eq!(run(&task, &b.id).state, UpdateRunState::Queued, "one workflow at a time");

        finished.store(true, Ordering::Relaxed);
        task.drive().await;
        assert_eq!(run(&task, &a.id).state, UpdateRunState::Watching);
        assert_eq!(run(&task, &a.id).step, None);
        assert_eq!(run(&task, &b.id).state, UpdateRunState::Running, "started without waiting for the watch");

        task.publish();
        let published: serde_json::Value = serde_json::from_str(&rx.borrow().json).unwrap();
        let ntfy = published["units"].as_array().unwrap().iter().find(|u| u["id"] == "binwiederhier/ntfy").unwrap();
        assert!(ntfy["run"]["watch_until"].as_i64().unwrap() > unix_now());
    }

    #[tokio::test]
    async fn an_update_goes_from_queued_to_done() {
        let api = fake_github().await;
        let (containers_tx, containers) = watch::channel(snapshot(ntfy("v2.28.0")));
        let (mut task, rx) = task(&api, containers, 0);
        let store = task.store.clone();
        task.tags = HashMap::from([("binwiederhier/ntfy".to_string(), vec!["v2.28.1".to_string(), "v2.29.0".to_string()])]);
        task.refresh_units();

        let refused = task.queue("binwiederhier/ntfy", Some("v9.0.0".into()), UpdateRunKind::Update, "riley").await;
        assert!(matches!(refused, Err(AgentError::BadRequest(_))), "{refused:?}");

        let run = task.queue("binwiederhier/ntfy", Some("v2.29.0".into()), UpdateRunKind::Update, "riley").await.unwrap();
        assert_eq!((run.from.as_str(), run.to.as_str(), run.state), ("v2.28.0", "v2.29.0", UpdateRunState::Queued));
        let again = task.queue("binwiederhier/ntfy", Some("v2.29.0".into()), UpdateRunKind::Update, "riley").await;
        assert!(again.is_err(), "one run per unit at a time");

        task.drive().await;
        assert_eq!(task.runs[0].run.state, UpdateRunState::Running);
        assert_eq!(task.runs[0].run.run_url.as_deref(), Some("https://github.com/o/r/actions/runs/42"));
        task.drive().await;
        assert_eq!(task.runs[0].run.state, UpdateRunState::Watching);
        task.drive().await;
        assert_eq!(task.runs[0].run.state, UpdateRunState::Failed, "the watch is over and ntfy still runs the old tag");

        // Again, with the deploy taking effect.
        let run = task.queue("binwiederhier/ntfy", Some("v2.29.0".into()), UpdateRunKind::Update, "riley").await.unwrap();
        task.drive().await;
        task.drive().await;
        containers_tx.send(snapshot(ntfy("v2.29.0"))).unwrap();
        task.refresh_units();
        task.drive().await;
        assert_eq!(task.runs[0].run.id, run.id);
        assert_eq!(task.runs[0].run.state, UpdateRunState::Done);

        // Stored, so a restart picks it up, and the rollback goes back to v2.28.0.
        let stored = store.with(|c| db::recent_runs(c, 5).unwrap());
        assert_eq!(stored[0].run.state, UpdateRunState::Done);
        let back = task.queue("binwiederhier/ntfy", None, UpdateRunKind::Rollback, "riley").await.unwrap();
        assert_eq!((back.from.as_str(), back.to.as_str()), ("v2.29.0", "v2.28.0"));

        task.publish();
        let published: serde_json::Value = serde_json::from_str(&rx.borrow().json).unwrap();
        assert_eq!(published["units"][0]["previous"], "v2.28.0");
        assert_eq!(published["history"].as_array().unwrap().len(), 3);
    }
}
