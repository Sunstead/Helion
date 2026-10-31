//! The decisions, kept pure: whether a deployed update is healthy, which
//! automatic updates to apply, and whether a backup was the nightly one.

use super::{ db::StoredRun, image::ImageRef, rules::Offer };
use crate::backups::parse_rfc3339;
use helion_common::types::{ BackupsStatus, CheckSource, CheckState, ChangeKind, ContainerInfo, UpdatePolicy, UptimeEntry };

/// A fresh container restarting this often has a problem.
const RESTARTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Wait,
    Done,
    /// Deployed, and then went wrong.
    Broken(String),
    /// Never took effect.
    Failed(String),
}

/// How a run looks during the watch after its deploy.
pub fn judge(run: &StoredRun, containers: &[ContainerInfo], uptime: &[UptimeEntry], now: i64) -> Verdict {
    let over = run.watch_until.is_some_and(|w| now >= w);
    let mut settled = true;
    for service in &run.services {
        let Some(c) = containers.iter().find(|c| c.compose_service.as_deref() == Some(service.as_str())) else {
            if over {
                return Verdict::Failed(format!("{service} isn't running after the deploy"));
            }
            settled = false;
            continue;
        };
        let tag = ImageRef::parse(&c.image).map(|i| i.tag).unwrap_or_default();
        if tag != run.run.to {
            if over {
                return Verdict::Failed(format!("{service} still runs {}", if tag.is_empty() { "another version" } else { &tag }));
            }
            settled = false;
            continue;
        }
        if c.state == "restarting" || c.restart_count >= RESTARTS {
            return Verdict::Broken(format!("{service} keeps restarting"));
        }
        if matches!(c.state.as_str(), "exited" | "dead") {
            return Verdict::Broken(format!("{service} stopped"));
        }
        if c.state != "running" {
            settled = false;
            continue;
        }
        let down = uptime.iter().find(|e| {
            e.check.source == CheckSource::Service &&
                e.state == CheckState::Down &&
                e.check.service.is_some() &&
                e.check.service == c.helion_service
        });
        if let Some(e) = down {
            let why = e.recent.last().and_then(|b| b.detail.clone());
            return Verdict::Broken(match why {
                Some(d) => format!("{} is down: {d}", e.check.name),
                None => format!("{} is down", e.check.name),
            });
        }
    }
    if over && settled { Verdict::Done } else { Verdict::Wait }
}

/// The tag an automatic update would take now, if any: the policy allows the
/// change, it isn't paused, and the tag has been out long enough for a bad
/// release to have been pulled.
pub fn auto_target(policy: UpdatePolicy, paused: bool, offer: &Offer, min_age_secs: i64, now: i64) -> Option<String> {
    if paused || offer.blocked.is_some() {
        return None;
    }
    let candidate = match policy {
        UpdatePolicy::Manual => None,
        UpdatePolicy::Patch =>
            offer.patch
                .as_ref()
                .or(offer.available.as_ref().filter(|a| a.change == ChangeKind::Patch)),
        UpdatePolicy::Auto => offer.available.as_ref(),
    }?;
    (now - candidate.first_seen >= min_age_secs).then(|| candidate.tag.clone())
}

/// A successful backup started by its timer, as opposed to one someone
/// started: the timer fired just before it began.
pub fn nightly_succeeded(s: &BackupsStatus) -> bool {
    let (Some(fired), Some(wrote)) = (
        s.timer_last_fired.as_deref().and_then(parse_rfc3339),
        parse_rfc3339(&s.generated_at),
    ) else {
        return false;
    };
    let took = s.duration_secs.unwrap_or(0) as i64;
    s.last_exit_code == Some(0) && wrote >= fired && wrote - fired <= took + 600
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::format_rfc3339;
    use helion_common::types::{
        CheckKind,
        UpdateCandidate,
        UpdateRun,
        UpdateRunKind,
        UpdateRunState,
        UptimeBeat,
        UptimeCheck,
        UptimeStats,
    };

    fn container(service: &str, image: &str, state: &str, restarts: u32) -> ContainerInfo {
        ContainerInfo {
            id: service.into(),
            name: service.into(),
            image: image.into(),
            status: String::new(),
            state: state.into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: restarts,
            compose_project: None,
            compose_service: Some(service.into()),
            update_labels: Default::default(),
            helion_service: Some("immich".into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
        }
    }

    fn run(watch_until: i64) -> StoredRun {
        StoredRun {
            run: UpdateRun {
                id: "r".into(),
                unit: "group:immich".into(),
                kind: UpdateRunKind::Update,
                from: "v3.2.2".into(),
                to: "v3.3.0".into(),
                by: "riley".into(),
                state: UpdateRunState::Watching,
                requested_at: 0,
                finished_at: None,
                run_url: None,
                detail: None,
                step: None,
                watch_until: None,
            },
            services: vec!["immich-server".into(), "immich-machine-learning".into()],
            run_id: None,
            watch_until: Some(watch_until),
        }
    }

    fn check(state: CheckState) -> UptimeEntry {
        UptimeEntry {
            check: UptimeCheck {
                id: "svc:immich".into(),
                source: CheckSource::Service,
                name: "immich".into(),
                kind: CheckKind::Http,
                target: "https://immich.example.net/".into(),
                interval_secs: 60,
                enabled: true,
                any_status: false,
                service: Some("immich".into()),
                via_peer: None,
            },
            state,
            since: None,
            recent: vec![UptimeBeat { at: 0, ok: false, latency_ms: None, detail: Some("HTTP 502".into()) }],
            stats: UptimeStats::default(),
            cert: None,
        }
    }

    const NEW_SERVER: &str = "ghcr.io/immich-app/immich-server:v3.3.0";
    const NEW_ML: &str = "ghcr.io/immich-app/immich-machine-learning:v3.3.0";

    #[test]
    fn healthy_through_the_watch_is_done() {
        let c = [container("immich-server", NEW_SERVER, "running", 0), container("immich-machine-learning", NEW_ML, "running", 0)];
        assert_eq!(judge(&run(600), &c, &[check(CheckState::Up)], 300), Verdict::Wait);
        assert_eq!(judge(&run(600), &c, &[check(CheckState::Up)], 600), Verdict::Done);
    }

    #[test]
    fn down_or_restarting_is_broken() {
        let c = [container("immich-server", NEW_SERVER, "running", 0), container("immich-machine-learning", NEW_ML, "running", 0)];
        assert_eq!(judge(&run(600), &c, &[check(CheckState::Down)], 300), Verdict::Broken("immich is down: HTTP 502".into()));
        let looping = [container("immich-server", NEW_SERVER, "restarting", 1), container("immich-machine-learning", NEW_ML, "running", 0)];
        assert_eq!(judge(&run(600), &looping, &[], 300), Verdict::Broken("immich-server keeps restarting".into()));
    }

    #[test]
    fn a_deploy_that_never_happened_fails_at_the_end() {
        let old = [container("immich-server", "ghcr.io/immich-app/immich-server:v3.2.2", "running", 0), container("immich-machine-learning", NEW_ML, "running", 0)];
        assert_eq!(judge(&run(600), &old, &[], 300), Verdict::Wait, "the deploy may still be pulling");
        assert_eq!(judge(&run(600), &old, &[], 600), Verdict::Failed("immich-server still runs v3.2.2".into()));
    }

    fn offer(available: Option<(&str, ChangeKind, i64)>, patch: Option<(&str, i64)>) -> Offer {
        Offer {
            available: available.map(|(t, change, seen)| UpdateCandidate { tag: t.into(), change, first_seen: seen }),
            patch: patch.map(|(t, seen)| UpdateCandidate { tag: t.into(), change: ChangeKind::Patch, first_seen: seen }),
            held: None,
            blocked: None,
        }
    }

    #[test]
    fn automatic_updates_follow_the_policy_and_the_minimum_age() {
        const DAY: i64 = 86_400;
        let o = offer(Some(("36.0.1-apache", ChangeKind::Major, 0)), Some(("35.0.2-apache", 0)));
        assert_eq!(auto_target(UpdatePolicy::Manual, false, &o, 3 * DAY, 10 * DAY), None);
        assert_eq!(auto_target(UpdatePolicy::Patch, false, &o, 3 * DAY, 10 * DAY).as_deref(), Some("35.0.2-apache"));
        assert_eq!(auto_target(UpdatePolicy::Auto, false, &o, 3 * DAY, 10 * DAY).as_deref(), Some("36.0.1-apache"));
        assert_eq!(auto_target(UpdatePolicy::Auto, true, &o, 3 * DAY, 10 * DAY), None, "paused");
        assert_eq!(auto_target(UpdatePolicy::Auto, false, &o, 3 * DAY, 2 * DAY), None, "too new");

        let only_patch = offer(Some(("v2.28.1", ChangeKind::Patch, 0)), None);
        assert_eq!(auto_target(UpdatePolicy::Patch, false, &only_patch, 0, 1).as_deref(), Some("v2.28.1"));
        let only_minor = offer(Some(("v2.29.0", ChangeKind::Minor, 0)), None);
        assert_eq!(auto_target(UpdatePolicy::Patch, false, &only_minor, 0, 1), None);
    }

    fn backup(exit: i32, fired: i64, wrote: i64, took: u64) -> BackupsStatus {
        BackupsStatus {
            generated_at: format_rfc3339(wrote),
            stale: false,
            last_run: None,
            next_run: None,
            timer_last_fired: Some(format_rfc3339(fired)),
            last_exit_code: Some(exit),
            duration_secs: Some(took),
            repo_label: String::new(),
            repo_size_bytes: None,
            snapshot_count: 0,
            snapshots: vec![],
            retention: None,
            postgres_dump: None,
            state_copy: None,
            space_check: None,
            heartbeat: None,
            requests: vec![],
            restore_test: None,
            databases: vec![],
        }
    }

    #[test]
    fn only_a_successful_timer_run_counts_as_the_nightly() {
        const NIGHT: i64 = 1_790_000_000;
        assert!(nightly_succeeded(&backup(0, NIGHT, NIGHT + 840, 840)));
        assert!(!nightly_succeeded(&backup(1, NIGHT, NIGHT + 840, 840)), "failed");
        assert!(!nightly_succeeded(&backup(0, NIGHT, NIGHT + 11 * 3_600, 600)), "someone started it in the afternoon");
    }
}
