//! Container events: crashes, out-of-memory kills, restart loops, health, and
//! deploys that change a container's image.
//!
//! **Asked or not.** Everything that stops a container on purpose (`docker
//! stop`, Compose recreating it during a deploy, a Helion action) sends a
//! `kill` event before the `die`. A `die` with no `kill` for that container in
//! the previous [`KILL_WINDOW`] is one nobody asked for. That's what keeps a
//! deploy, which recreates every container it touches, from reading as a wave
//! of crashes.
//!
//! One-off events come from the Docker event stream as they happen. Problems
//! are checked against the container list on a tick, which also resolves the
//! ones a restart of the agent left open.

use super::{ EventsHandle, NewEvent, ProblemSpec, Report, Resolution };
use crate::{ backups::parse_rfc3339, sample::{ docker::ContainerSnapshot, host::unix_now, Forwarded } };
use bollard::models::{ EventMessage, EventMessageTypeEnum };
use helion_common::types::{ ContainerHealth, ContainerInfo, EventCategory, Problem, Severity };
use std::{ collections::HashMap, sync::Arc, time::Duration };
use tokio::sync::{ mpsc, watch };

/// A `kill` this recent makes the `die` that follows one somebody asked for.
pub const KILL_WINDOW: i64 = 30;
/// A container that died unasked and is still not running after this long is
/// down. Shorter would flag every container a restart policy brings back.
const DOWN_AFTER: i64 = 60;
/// This many unasked exits within [`LOOP_WINDOW`] is a crash loop.
const LOOP_EXITS: usize = 3;
const LOOP_WINDOW: i64 = 600;
/// A crash loop clears once the container has run this long without dying.
const STABLE_FOR: i64 = 600;
/// A container missing from the list this long has been removed, not
/// recreated (Compose destroys the old one a moment before creating the new).
const GONE_AFTER: i64 = 60;
const TICK: Duration = Duration::from_secs(15);

/// The parts of a Docker event this cares about.
#[derive(Debug, Clone, PartialEq)]
pub struct DockerEvent {
    pub action: String,
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub exit_code: Option<i64>,
    pub service: Option<String>,
}

impl DockerEvent {
    pub fn from_message(m: &EventMessage) -> Option<Self> {
        if m.typ != Some(EventMessageTypeEnum::CONTAINER) {
            return None;
        }
        let actor = m.actor.as_ref()?;
        let attrs = actor.attributes.as_ref();
        let attr = |k: &str| attrs.and_then(|a| a.get(k)).cloned();
        Some(Self {
            action: m.action.clone()?,
            id: actor.id.clone()?,
            name: attr("name")?,
            image: attr("image"),
            exit_code: attr("exitCode").and_then(|c| c.parse().ok()),
            service: attrs.and_then(|a| crate::labels::label(a, "service")),
        })
    }
}

struct Died {
    at: i64,
    detail: String,
    service: Option<String>,
}

#[derive(Default)]
pub struct ContainerTracker {
    /// Container id → when a `kill` was seen.
    killed: HashMap<String, i64>,
    /// Container id → when an `oom` was seen.
    oom: HashMap<String, i64>,
    /// Name → times of recent unasked exits, for crash loops.
    exits: HashMap<String, Vec<i64>>,
    /// Name → an unasked exit that hasn't become `down` yet.
    died: HashMap<String, Died>,
    /// Name → the image it runs, to notice a deploy changing it.
    images: HashMap<String, String>,
    /// Name → since when a container with an open problem has been missing.
    missing: HashMap<String, i64>,
}

fn key(name: &str, kind: &str) -> String {
    format!("container:{name}:{kind}")
}

/// Names the signal for exit codes above 128, which is how a process killed
/// by a signal exits.
fn exit_detail(code: i64) -> String {
    let why = match code {
        134 => " (aborted)",
        137 => " (killed)",
        139 => " (segmentation fault)",
        143 => " (terminated)",
        _ => "",
    };
    format!("Exited with code {code}{why}.")
}

impl ContainerTracker {
    /// An event from before the stream was (re)opened, at its own time. Only
    /// what explains a later `die` is kept: whoever was listening then
    /// already reported the rest, and reporting it again would repeat it.
    pub fn replay(&mut self, e: &DockerEvent, at: i64) {
        match e.action.as_str() {
            "kill" => {
                self.killed.insert(e.id.clone(), at);
            }
            "oom" => {
                self.oom.insert(e.id.clone(), at);
            }
            "destroy" => {
                self.killed.remove(&e.id);
                self.oom.remove(&e.id);
            }
            _ => {}
        }
    }

    pub fn on_event(&mut self, e: &DockerEvent, now: i64) -> Vec<Report> {
        let mut out = Vec::new();
        match e.action.as_str() {
            "kill" => {
                self.killed.insert(e.id.clone(), now);
            }
            "oom" => {
                self.oom.insert(e.id.clone(), now);
            }
            "die" => {
                let asked = self.killed.remove(&e.id).is_some_and(|t| now - t <= KILL_WINDOW);
                let oom = self.oom.remove(&e.id).is_some_and(|t| now - t <= KILL_WINDOW);
                if asked && !oom {
                    return out;
                }
                let code = e.exit_code.unwrap_or(-1);
                if !oom && code == 0 {
                    out.push(
                        Report::Event(
                            NewEvent::new(EventCategory::Container, "exited", Severity::Info, &e.name, format!("{} exited", e.name))
                                .detail("It stopped on its own with exit code 0.")
                                .service(e.service.clone())
                        )
                    );
                    return out;
                }

                let (kind, title, detail) = if oom {
                    ("oom_killed", format!("{} ran out of memory", e.name), "The kernel killed it for using too much memory.".to_string())
                } else {
                    ("crashed", format!("{} crashed (exit {code})", e.name), exit_detail(code))
                };
                out.push(
                    Report::Event(
                        NewEvent::new(EventCategory::Container, kind, Severity::Error, &e.name, title)
                            .detail(detail.clone())
                            .service(e.service.clone())
                    )
                );

                let exits = self.exits.entry(e.name.clone()).or_default();
                exits.retain(|t| now - t < LOOP_WINDOW);
                exits.push(now);
                if exits.len() >= LOOP_EXITS {
                    out.push(
                        Report::Open(ProblemSpec {
                            key: key(&e.name, "crash_loop"),
                            category: EventCategory::Container,
                            kind: "crash_loop",
                            severity: Severity::Error,
                            subject: e.name.clone(),
                            title: format!("{} keeps crashing", e.name),
                            detail: Some(format!("{} crashes in the last {} minutes. {detail}", exits.len(), LOOP_WINDOW / 60)),
                            service: e.service.clone(),
                            depends_on: None,
                        })
                    );
                }
                self.died.insert(e.name.clone(), Died { at: now, detail, service: e.service.clone() });
            }
            "start" => {
                self.died.remove(&e.name);
                if let Some(image) = &e.image {
                    if let Some(previous) = self.images.insert(e.name.clone(), image.clone()) {
                        if &previous != image {
                            out.push(
                                Report::Event(
                                    NewEvent::new(
                                        EventCategory::Container,
                                        "image_changed",
                                        Severity::Info,
                                        &e.name,
                                        format!("{} updated", e.name)
                                    )
                                        .detail(format!("Now runs {image}, was {previous}."))
                                        .service(e.service.clone())
                                )
                            );
                        }
                    }
                }
            }
            "destroy" => {
                self.killed.remove(&e.id);
                self.oom.remove(&e.id);
            }
            _ => {}
        }
        out
    }

    pub fn on_tick(&mut self, containers: &[ContainerInfo], open: &[Problem], now: i64) -> Vec<Report> {
        let mut out = Vec::new();
        let by_name: HashMap<&str, &ContainerInfo> = containers
            .iter()
            .map(|c| (c.name.as_str(), c))
            .collect();

        // Known images, so the next start with a different one is a deploy.
        for c in containers {
            self.images.entry(c.name.clone()).or_insert_with(|| c.image.clone());
        }

        // Unasked exits that haven't come back.
        let mut down = Vec::new();
        self.died.retain(|name, d| {
            match by_name.get(name.as_str()) {
                Some(c) if c.state == "running" => false,
                None => false,
                Some(c) if now - d.at >= DOWN_AFTER => {
                    down.push(ProblemSpec {
                        key: key(name, "down"),
                        category: EventCategory::Container,
                        kind: "down",
                        severity: Severity::Error,
                        subject: name.clone(),
                        title: format!("{name} is down"),
                        detail: Some(format!("{} It hasn't restarted.", d.detail)),
                        service: c.helion_service.clone().or_else(|| d.service.clone()),
                        depends_on: None,
                    });
                    false
                }
                Some(_) => true,
            }
        });
        out.extend(down.into_iter().map(Report::Open));

        for c in containers {
            if c.state == "running" && c.health == Some(ContainerHealth::Unhealthy) {
                out.push(
                    Report::Open(ProblemSpec {
                        key: key(&c.name, "unhealthy"),
                        category: EventCategory::Container,
                        kind: "unhealthy",
                        severity: Severity::Warning,
                        subject: c.name.clone(),
                        title: format!("{} is unhealthy", c.name),
                        detail: Some("Its health check is failing.".into()),
                        service: c.helion_service.clone(),
                        depends_on: None,
                    })
                );
            }
        }

        // Resolve what's cleared, including problems left open across a
        // restart of the agent.
        let mut still_missing = HashMap::new();
        for p in open.iter().filter(|p| p.category == EventCategory::Container) {
            let name = p.subject.as_str();
            let resolved = |title: String| {
                Report::Resolve(Resolution { key: p.key.clone(), title, detail: None })
            };
            let Some(c) = by_name.get(name) else {
                let since = *self.missing.get(name).unwrap_or(&now);
                still_missing.insert(name.to_string(), since);
                if now - since >= GONE_AFTER {
                    out.push(resolved(format!("{name} was removed")));
                }
                continue;
            };
            let running = c.state == "running";
            match p.kind.as_str() {
                "unhealthy" =>
                    match (running, c.health) {
                        (true, Some(ContainerHealth::Unhealthy | ContainerHealth::Starting)) => {}
                        (true, Some(ContainerHealth::Healthy)) => out.push(resolved(format!("{name} is healthy again"))),
                        (true, None) => out.push(resolved(format!("{name} no longer has a health check"))),
                        (false, _) => out.push(resolved(format!("{name} stopped"))),
                    }
                "down" if running => out.push(resolved(format!("{name} is running again"))),
                "crash_loop" if running => {
                    let quiet = self.exits.get(name).is_none_or(|e| e.iter().all(|t| now - t >= STABLE_FOR));
                    let up_for = c.started_at
                        .as_deref()
                        .and_then(parse_rfc3339)
                        .map(|t| now - t);
                    if quiet && up_for.is_none_or(|u| u >= STABLE_FOR) {
                        out.push(resolved(format!("{name} has stopped crashing")));
                    }
                }
                _ => {}
            }
        }
        self.missing = still_missing;
        out
    }
}

/// Feeds the tracker from the Docker event stream and a tick over the
/// container list.
pub fn spawn(
    events: EventsHandle,
    mut docker_events: mpsc::Receiver<Forwarded>,
    containers: watch::Receiver<Arc<ContainerSnapshot>>
) {
    tokio::spawn(async move {
        let mut tracker = ContainerTracker::default();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = docker_events.recv() => {
                    let Some(msg) = msg else { return; };
                    let Some(e) = DockerEvent::from_message(&msg.event) else { continue; };
                    if msg.replayed {
                        tracker.replay(&e, msg.event.time.unwrap_or_else(unix_now));
                    } else {
                        events.report_all(tracker.on_event(&e, unix_now()));
                    }
                }
                _ = tick.tick() => {
                    let snap = containers.borrow().clone();
                    // Nothing sampled yet: an empty list here isn't an empty host.
                    if snap.sampled_at == 0 {
                        continue;
                    }
                    events.report_all(tracker.on_tick(&snap.containers, &events.open_problems(), unix_now()));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(action: &str, id: &str, name: &str) -> DockerEvent {
        DockerEvent {
            action: action.into(),
            id: id.into(),
            name: name.into(),
            image: None,
            exit_code: None,
            service: Some(name.into()),
        }
    }

    fn die(id: &str, name: &str, code: i64) -> DockerEvent {
        DockerEvent { exit_code: Some(code), ..ev("die", id, name) }
    }

    fn start(id: &str, name: &str, image: &str) -> DockerEvent {
        DockerEvent { image: Some(image.into()), ..ev("start", id, name) }
    }

    fn container(name: &str, state: &str, health: Option<ContainerHealth>) -> ContainerInfo {
        ContainerInfo {
            id: format!("{name}-id"),
            name: name.into(),
            image: format!("{name}:1"),
            status: String::new(),
            state: state.into(),
            health,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: None,
            compose_service: None,
            update_labels: Default::default(),
            helion_service: Some(name.into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
        }
    }

    /// Plays events at the given times and returns everything reported.
    fn play(t: &mut ContainerTracker, events: &[(i64, DockerEvent)]) -> Vec<Report> {
        events
            .iter()
            .flat_map(|(at, e)| t.on_event(e, *at))
            .collect()
    }

    fn kinds(reports: &[Report]) -> Vec<String> {
        reports
            .iter()
            .map(|r| {
                match r {
                    Report::Event(e) => format!("event:{}", e.kind),
                    Report::Open(p) => format!("open:{}", p.kind),
                    Report::Resolve(r) => format!("resolve:{}", r.key),
                }
            })
            .collect()
    }

    fn problem(name: &str, kind: &'static str) -> Problem {
        Problem {
            key: key(name, kind),
            category: EventCategory::Container,
            kind: kind.into(),
            severity: Severity::Error,
            subject: name.into(),
            title: String::new(),
            detail: None,
            service: None,
            opened_at: 0,
            event_id: 1,
            depends_on: None,
        }
    }

    /// `docker compose up` recreating a container: kill, die, stop, destroy,
    /// then a new container with the same name on a new image.
    #[test]
    fn a_deploy_is_an_update_not_a_crash() {
        let mut t = ContainerTracker::default();
        t.on_tick(&[container("gitea", "running", None)], &[], 0);
        let out = play(&mut t, &[
            (100, ev("kill", "old", "gitea")),
            (101, die("old", "gitea", 143)),
            (101, ev("stop", "old", "gitea")),
            (102, ev("destroy", "old", "gitea")),
            (103, ev("create", "new", "gitea")),
            (104, start("new", "gitea", "gitea:2")),
        ]);
        assert_eq!(kinds(&out), ["event:image_changed"]);
        let Report::Event(e) = &out[0] else { unreachable!() };
        assert_eq!(e.detail.as_deref(), Some("Now runs gitea:2, was gitea:1."));
    }

    #[test]
    fn a_stop_or_restart_someone_asked_for_is_quiet() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[
            (100, ev("kill", "a", "gitea")),
            (101, die("a", "gitea", 0)),
            (200, ev("kill", "a", "gitea")),
            (201, die("a", "gitea", 137)),
            (202, start("a", "gitea", "gitea:1")),
        ]);
        assert!(out.is_empty(), "{:?}", kinds(&out));
    }

    /// A deploy recreates the agent too: the new one subscribes between
    /// another container's `kill` and its `die`, and the replay supplies the
    /// `kill` it missed.
    #[test]
    fn a_kill_from_before_the_agent_restarted_still_counts() {
        let mut t = ContainerTracker::default();
        t.replay(&ev("kill", "a", "immich"), 100);
        t.replay(&die("b", "gitea", 1), 101);
        t.replay(&start("b", "gitea", "gitea:1"), 102);
        let out = play(&mut t, &[(106, die("a", "immich", 143))]);
        assert!(out.is_empty(), "{:?}", kinds(&out));
        assert!(t.died.is_empty(), "a replayed die was already reported");
    }

    #[test]
    fn a_die_long_after_a_kill_is_still_a_crash() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[(100, ev("kill", "a", "gitea")), (100 + KILL_WINDOW + 1, die("a", "gitea", 1))]);
        assert_eq!(kinds(&out), ["event:crashed"]);
    }

    #[test]
    fn an_unasked_exit_is_a_crash_and_zero_is_just_an_exit() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[(100, die("a", "gitea", 139)), (200, die("b", "job", 0))]);
        assert_eq!(kinds(&out), ["event:crashed", "event:exited"]);
        let Report::Event(crash) = &out[0] else { unreachable!() };
        assert_eq!(crash.title, "gitea crashed (exit 139)");
        assert_eq!(crash.detail.as_deref(), Some("Exited with code 139 (segmentation fault)."));
        assert_eq!(crash.severity, Severity::Error);
        let Report::Event(exit) = &out[1] else { unreachable!() };
        assert_eq!(exit.severity, Severity::Info);
    }

    #[test]
    fn out_of_memory_is_named_even_after_a_kill() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[(100, ev("oom", "a", "immich")), (100, die("a", "immich", 137))]);
        assert_eq!(kinds(&out), ["event:oom_killed"]);
    }

    #[test]
    fn three_crashes_in_ten_minutes_open_a_crash_loop_that_clears_when_quiet() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[
            (0, die("a", "gitea", 1)),
            (5, start("a", "gitea", "gitea:1")),
            (60, die("a", "gitea", 1)),
            (65, start("a", "gitea", "gitea:1")),
            (120, die("a", "gitea", 1)),
            (125, start("a", "gitea", "gitea:1")),
        ]);
        assert_eq!(kinds(&out), ["event:crashed", "event:crashed", "event:crashed", "open:crash_loop"]);

        let open = [problem("gitea", "crash_loop")];
        let running = [container("gitea", "running", None)];
        assert!(t.on_tick(&running, &open, 120 + STABLE_FOR - 1).is_empty(), "not quiet long enough");
        assert_eq!(kinds(&t.on_tick(&running, &open, 120 + STABLE_FOR)), ["resolve:container:gitea:crash_loop"]);
    }

    #[test]
    fn crashes_spread_out_are_not_a_loop() {
        let mut t = ContainerTracker::default();
        let out = play(&mut t, &[(0, die("a", "gitea", 1)), (400, die("a", "gitea", 1)), (800, die("a", "gitea", 1))]);
        assert_eq!(kinds(&out), ["event:crashed", "event:crashed", "event:crashed"]);
    }

    #[test]
    fn a_crash_that_stays_down_opens_a_problem_after_a_minute() {
        let mut t = ContainerTracker::default();
        play(&mut t, &[(100, die("a", "gitea", 1))]);
        let exited = [container("gitea", "exited", None)];
        assert!(t.on_tick(&exited, &[], 100 + DOWN_AFTER - 1).is_empty());
        let out = t.on_tick(&exited, &[], 100 + DOWN_AFTER);
        assert_eq!(kinds(&out), ["open:down"]);
        let Report::Open(p) = &out[0] else { unreachable!() };
        assert_eq!(p.detail.as_deref(), Some("Exited with code 1. It hasn't restarted."));
        assert!(t.on_tick(&exited, &[], 500).is_empty(), "opened once, not every tick");

        let open = [problem("gitea", "down")];
        let out = t.on_tick(&[container("gitea", "running", None)], &open, 600);
        assert_eq!(kinds(&out), ["resolve:container:gitea:down"]);
    }

    #[test]
    fn a_restart_policy_bringing_it_back_is_not_down() {
        let mut t = ContainerTracker::default();
        play(&mut t, &[(100, die("a", "gitea", 1)), (102, start("a", "gitea", "gitea:1"))]);
        assert!(t.on_tick(&[container("gitea", "running", None)], &[], 300).is_empty());
    }

    #[test]
    fn health_opens_and_clears() {
        let mut t = ContainerTracker::default();
        let sick = [container("gitea", "running", Some(ContainerHealth::Unhealthy))];
        assert_eq!(kinds(&t.on_tick(&sick, &[], 0)), ["open:unhealthy"]);

        let open = [problem("gitea", "unhealthy")];
        let starting = [container("gitea", "running", Some(ContainerHealth::Starting))];
        assert!(t.on_tick(&starting, &open, 10).is_empty(), "wait for the check to finish");
        let healthy = [container("gitea", "running", Some(ContainerHealth::Healthy))];
        assert_eq!(kinds(&t.on_tick(&healthy, &open, 20)), ["resolve:container:gitea:unhealthy"]);
    }

    #[test]
    fn a_removed_container_resolves_only_once_it_stays_gone() {
        let mut t = ContainerTracker::default();
        let open = [problem("gitea", "unhealthy")];
        assert!(t.on_tick(&[], &open, 100).is_empty(), "maybe mid-recreate");
        assert!(t.on_tick(&[], &open, 100 + GONE_AFTER - 1).is_empty());
        let out = t.on_tick(&[], &open, 100 + GONE_AFTER);
        assert_eq!(kinds(&out), ["resolve:container:gitea:unhealthy"]);
        let Report::Resolve(r) = &out[0] else { unreachable!() };
        assert_eq!(r.title, "gitea was removed");
    }

    #[test]
    fn events_are_read_from_docker_messages() {
        use bollard::models::EventActor;
        let msg = EventMessage {
            typ: Some(EventMessageTypeEnum::CONTAINER),
            action: Some("die".into()),
            actor: Some(EventActor {
                id: Some("abc".into()),
                attributes: Some(
                    [
                        ("name".to_string(), "gitea".to_string()),
                        ("image".to_string(), "gitea/gitea:1.27.3".to_string()),
                        ("exitCode".to_string(), "2".to_string()),
                        ("helion.service".to_string(), "gitea".to_string()),
                    ]
                        .into_iter()
                        .collect()
                ),
            }),
            ..Default::default()
        };
        let e = DockerEvent::from_message(&msg).unwrap();
        assert_eq!(e.exit_code, Some(2));
        assert_eq!(e.service.as_deref(), Some("gitea"));
        assert_eq!(e.image.as_deref(), Some("gitea/gitea:1.27.3"));

        let mut legacy = msg.clone();
        let attrs = legacy.actor.as_mut().and_then(|a| a.attributes.as_mut()).unwrap();
        attrs.remove("helion.service");
        attrs.insert("cosmos.service".into(), "gitea".into());
        assert_eq!(DockerEvent::from_message(&legacy).unwrap().service.as_deref(), Some("gitea"));

        let volume = EventMessage { typ: Some(EventMessageTypeEnum::VOLUME), ..msg };
        assert_eq!(DockerEvent::from_message(&volume), None);
    }
}
