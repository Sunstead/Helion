//! Which services update together, and which newer tags their labels allow.
//!
//! A unit is every service running one image repository (Authentik's server
//! and worker, both Postgres databases), or an explicit
//! `helion.update.group`, which joins different repositories that must run
//! the same version (Immich's server and machine learning). Rules come from
//! the services' labels, in git:
//!
//! | Label | Meaning |
//! |---|---|
//! | `helion.update: off` | never offered |
//! | `helion.update.group: <name>` | these move together, to one tag |
//! | `helion.update.hold: major` (or `minor`) | nothing that big is offered |
//! | `helion.update.major-step: 1` | at most that many majors at once |
//! | `helion.update.major-digits: 2` | the first two numbers are the major |
//! | `helion.update.backup: true` | has a database: back up the state first |
//!
//! Where members disagree, the stricter rule wins; a disagreement that has
//! no stricter side (the major's width) blocks the unit.

use super::{ image::ImageRef, tags::Version };
use helion_common::types::{ ChangeKind, ContainerInfo, HeldUpdate, UpdateCandidate };
use std::collections::{ BTreeMap, HashMap };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub service: String,
    pub image: ImageRef,
    pub helion_service: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub off: bool,
    /// The smallest change that's held back.
    pub hold: Option<ChangeKind>,
    pub major_step: Option<u64>,
    pub major_digits: usize,
    pub backup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub id: String,
    pub name: String,
    pub members: Vec<Member>,
    pub rules: Rules,
    /// The tag every member runs.
    pub current: String,
    pub blocked: Option<String>,
}

impl Unit {
    /// Each repository once, in order.
    pub fn images(&self) -> Vec<&ImageRef> {
        let mut out: Vec<&ImageRef> = Vec::new();
        for m in &self.members {
            if !out.iter().any(|i| i.repo == m.image.repo) {
                out.push(&m.image);
            }
        }
        out
    }

    pub fn services(&self) -> Vec<String> {
        self.members
            .iter()
            .map(|m| m.service.clone())
            .collect()
    }
}

fn merge_rules(labels: &[&BTreeMap<String, String>]) -> (Rules, Option<String>) {
    let mut rules = Rules { major_digits: 1, ..Rules::default() };
    let mut digits: Option<usize> = None;
    let mut blocked = None;
    for l in labels {
        let get = |k: &str| l.get(k).map(|v| v.trim().to_ascii_lowercase());
        if get("helion.update").is_some_and(|v| v == "off" || v == "false") {
            rules.off = true;
        }
        match get("helion.update.hold").as_deref() {
            Some("minor") => rules.hold = Some(ChangeKind::Minor),
            Some("major") if rules.hold.is_none() => rules.hold = Some(ChangeKind::Major),
            _ => {}
        }
        if let Some(n) = get("helion.update.major-step").and_then(|v| v.parse::<u64>().ok()).filter(|n| *n > 0) {
            rules.major_step = Some(rules.major_step.map_or(n, |m| m.min(n)));
        }
        if let Some(d) = get("helion.update.major-digits").and_then(|v| v.parse::<usize>().ok()).filter(|d| (1..=3).contains(d)) {
            match digits {
                Some(existing) if existing != d => {
                    blocked = Some("its services disagree on helion.update.major-digits".to_string());
                }
                _ => digits = Some(d),
            }
        }
        if get("helion.update.backup").is_some_and(|v| v == "true" || v == "yes") {
            rules.backup = true;
        }
    }
    rules.major_digits = digits.unwrap_or(1);
    (rules, blocked)
}

/// Groups the containers into units. Containers without a compose service
/// or a tagged image can't be updated through compose and are left out.
pub fn units(containers: &[ContainerInfo]) -> Vec<Unit> {
    let mut by_key: BTreeMap<String, Vec<(&ContainerInfo, ImageRef)>> = BTreeMap::new();
    let mut seen_services: Vec<&str> = Vec::new();
    for c in containers {
        let (Some(service), Some(image)) = (c.compose_service.as_deref(), ImageRef::parse(&c.image)) else {
            continue;
        };
        if seen_services.contains(&service) {
            continue;
        }
        seen_services.push(service);
        let key = match c.update_labels.get("helion.update.group").map(|g| g.trim()).filter(|g| !g.is_empty()) {
            Some(group) => format!("group:{group}"),
            None => image.repo.clone(),
        };
        by_key.entry(key).or_default().push((c, image));
    }

    by_key
        .into_iter()
        .map(|(id, members)| {
            let labels: Vec<&BTreeMap<String, String>> = members
                .iter()
                .map(|(c, _)| &c.update_labels)
                .collect();
            let (rules, mut blocked) = merge_rules(&labels);
            let tags: Vec<&str> = members
                .iter()
                .map(|(_, i)| i.tag.as_str())
                .collect();
            if tags.iter().any(|t| *t != tags[0]) {
                let mut distinct = tags.clone();
                distinct.dedup();
                blocked = Some(format!("its services run different versions ({})", distinct.join(", ")));
            }
            if rules.off {
                blocked = Some("updates are off for it (helion.update: off)".into());
            }
            let helion: Vec<Option<&str>> = members
                .iter()
                .map(|(c, _)| c.helion_service.as_deref())
                .collect();
            let name = match helion[0] {
                Some(s) if s != "system" && helion.iter().all(|c| *c == Some(s)) => s.to_string(),
                _ =>
                    match id.strip_prefix("group:") {
                        Some(g) => g.to_string(),
                        None => id.rsplit('/').next().unwrap_or(&id).to_string(),
                    }
            };
            Unit {
                name,
                current: tags[0].to_string(),
                members: members
                    .iter()
                    .map(|(c, image)| Member {
                        service: c.compose_service.clone().unwrap_or_default(),
                        image: image.clone(),
                        helion_service: c.helion_service.clone(),
                    })
                    .collect(),
                id,
                rules,
                blocked,
            }
        })
        .collect()
}

/// What a unit can move to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offer {
    pub available: Option<UpdateCandidate>,
    pub patch: Option<UpdateCandidate>,
    pub held: Option<HeldUpdate>,
    /// Why nothing can be offered, when that's the case.
    pub blocked: Option<String>,
}

/// Newer tags for `unit` from each repository's tag list, and when this agent
/// first saw each. A group's tag must exist in every repository.
pub fn offer(unit: &Unit, tags: &HashMap<String, Vec<String>>, first_seen: &HashMap<(String, String), i64>) -> Offer {
    if let Some(b) = &unit.blocked {
        return Offer { blocked: Some(b.clone()), ..Offer::default() };
    }
    let Some(current) = Version::parse(&unit.current) else {
        return Offer { blocked: Some(format!("{} isn't a version number", unit.current)), ..Offer::default() };
    };
    let images = unit.images();
    let Some(first) = tags.get(&images[0].repo) else {
        return Offer::default();
    };
    let digits = unit.rules.major_digits;

    // (version, tag, change, first seen)
    let mut candidates: Vec<(Version, &str, ChangeKind, i64)> = first
        .iter()
        .filter(|t| images[1..].iter().all(|i| tags.get(&i.repo).is_some_and(|list| list.contains(t))))
        .filter_map(|t| {
            let v = Version::parse(t)?;
            let change = current.change_to(&v, digits)?;
            let seen = images
                .iter()
                .filter_map(|i| first_seen.get(&(i.repo.clone(), t.clone())))
                .max()
                .copied()
                .unwrap_or(i64::MAX);
            Some((v, t.as_str(), change, seen))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.nums.cmp(&a.0.nums));

    let why_held = |(v, _, change, _): &(Version, &str, ChangeKind, i64)| -> Option<String> {
        match unit.rules.hold {
            Some(ChangeKind::Minor) if *change != ChangeKind::Patch => {
                return Some("only patches are allowed (helion.update.hold: minor)".into());
            }
            Some(ChangeKind::Major) if *change == ChangeKind::Major => {
                return Some("major versions are held (helion.update.hold: major)".into());
            }
            _ => {}
        }
        match unit.rules.major_step {
            Some(n) if *change == ChangeKind::Major && current.majors_ahead(v, digits) > n => {
                Some(if n == 1 { "one major version at a time".into() } else { format!("{n} major versions at a time") })
            }
            _ => None,
        }
    };
    let candidate = |c: &(Version, &str, ChangeKind, i64)| UpdateCandidate { tag: c.1.to_string(), change: c.2, first_seen: c.3 };

    let allowed = candidates.iter().find(|c| why_held(c).is_none());
    let held = candidates
        .first()
        .filter(|newest| allowed.is_none_or(|a| a.1 != newest.1))
        .and_then(|newest| why_held(newest).map(|reason| HeldUpdate { tag: newest.1.to_string(), reason }));
    let patch = candidates.iter().find(|c| c.2 == ChangeKind::Patch && why_held(c).is_none());
    Offer {
        available: allowed.map(candidate),
        patch: patch.filter(|p| allowed.is_some_and(|a| a.1 != p.1)).map(candidate),
        held,
        blocked: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(service: &str, image: &str, helion: Option<&str>, labels: &[(&str, &str)]) -> ContainerInfo {
        ContainerInfo {
            id: service.into(),
            name: service.into(),
            image: image.into(),
            status: String::new(),
            state: "running".into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: Some("jupiter".into()),
            compose_service: Some(service.into()),
            update_labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            helion_service: helion.map(Into::into),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_limit_bytes: 0,
        }
    }

    /// Jupiter's stack, with the labels its compose files get.
    fn jupiter() -> Vec<ContainerInfo> {
        let backup = ("helion.update.backup", "true");
        vec![
            container("postgres", "postgres:16.15-alpine", Some("system"), &[("helion.update.hold", "major"), backup]),
            container("authentik-postgres", "postgres:16.15-alpine", Some("authentik"), &[("helion.update.hold", "major"), backup]),
            container("authentik-server", "ghcr.io/goauthentik/server:2026.5.2", Some("authentik"), &[("helion.update.major-digits", "2"), backup]),
            container("authentik-worker", "ghcr.io/goauthentik/server:2026.5.2", Some("authentik"), &[("helion.update.major-digits", "2"), backup]),
            container("immich-server", "ghcr.io/immich-app/immich-server:v3.2.2", Some("immich"), &[("helion.update.group", "immich"), backup]),
            container("immich-machine-learning", "ghcr.io/immich-app/immich-machine-learning:v3.2.2", Some("immich"), &[("helion.update.group", "immich"), backup]),
            container("immich-postgres", "ghcr.io/immich-app/postgres:18-vectorchord0.5.3-pgvector0.8.1", Some("immich"), &[("helion.update", "off")]),
            container("nextcloud", "nextcloud:35.0.0-apache", Some("nextcloud"), &[("helion.update.major-step", "1"), backup]),
            container("nextcloud-cron", "nextcloud:35.0.0-apache", Some("nextcloud"), &[("helion.update.major-step", "1"), backup]),
            container("ntfy", "binwiederhier/ntfy:v2.28.0", Some("ntfy"), &[]),
            container("adhoc", "sha256:0123", None, &[]),
        ]
    }

    fn unit<'a>(units: &'a [Unit], id: &str) -> &'a Unit {
        units.iter().find(|u| u.id == id).unwrap_or_else(|| panic!("no unit {id}"))
    }

    #[test]
    fn groups_services_like_renovate_did() {
        let units = units(&jupiter());
        let ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, [
            "binwiederhier/ntfy",
            "ghcr.io/goauthentik/server",
            "ghcr.io/immich-app/postgres",
            "group:immich",
            "nextcloud",
            "postgres",
        ]);
        let pg = unit(&units, "postgres");
        assert_eq!(pg.services(), ["postgres", "authentik-postgres"]);
        assert_eq!(pg.name, "postgres", "a unit spanning two services is named after its image");
        assert_eq!(pg.rules.hold, Some(ChangeKind::Major));
        assert!(pg.rules.backup);

        let immich = unit(&units, "group:immich");
        assert_eq!(immich.name, "immich");
        assert_eq!(immich.images().len(), 2);
        assert_eq!(unit(&units, "ghcr.io/goauthentik/server").rules.major_digits, 2);
        assert!(unit(&units, "ghcr.io/immich-app/postgres").blocked.is_some());
        assert!(!unit(&units, "binwiederhier/ntfy").rules.backup);
    }

    fn lists(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(repo, tags)| (repo.to_string(), tags.iter().map(|t| t.to_string()).collect()))
            .collect()
    }

    #[test]
    fn offers_the_newest_allowed_and_says_what_is_held() {
        let units = units(&jupiter());
        let none = HashMap::new();

        let pg = offer(unit(&units, "postgres"), &lists(&[("postgres", &["16.14-alpine", "16.16-alpine", "17.6-alpine", "16.16", "latest"])]), &none);
        assert_eq!(pg.available.as_ref().map(|c| (c.tag.as_str(), c.change)), Some(("16.16-alpine", ChangeKind::Patch)));
        assert_eq!(pg.held.map(|h| (h.tag, h.reason)), Some(("17.6-alpine".into(), "major versions are held (helion.update.hold: major)".into())));

        let nc = offer(unit(&units, "nextcloud"), &lists(&[("nextcloud", &["35.0.1-apache", "36.0.3-apache", "37.0.0-apache"])]), &none);
        assert_eq!(nc.available.as_ref().map(|c| c.tag.as_str()), Some("36.0.3-apache"));
        assert_eq!(nc.patch.as_ref().map(|c| c.tag.as_str()), Some("35.0.1-apache"));
        assert_eq!(nc.held.map(|h| h.reason), Some("one major version at a time".into()));

        let ak = offer(unit(&units, "ghcr.io/goauthentik/server"), &lists(&[("ghcr.io/goauthentik/server", &["2026.5.3", "2026.8.3"])]), &none);
        assert_eq!(ak.available.map(|c| (c.tag, c.change)), Some(("2026.8.3".into(), ChangeKind::Major)));
        assert_eq!(ak.patch.map(|c| c.tag), Some("2026.5.3".into()));
    }

    #[test]
    fn a_group_only_moves_to_a_tag_every_image_has() {
        let units = units(&jupiter());
        let seen = HashMap::from([
            (("ghcr.io/immich-app/immich-server".to_string(), "v3.3.0".to_string()), 100),
            (("ghcr.io/immich-app/immich-machine-learning".to_string(), "v3.3.0".to_string()), 250),
        ]);
        let tags = lists(&[
            ("ghcr.io/immich-app/immich-server", &["v3.3.0", "v3.3.1"]),
            ("ghcr.io/immich-app/immich-machine-learning", &["v3.3.0"]),
        ]);
        let o = offer(unit(&units, "group:immich"), &tags, &seen);
        let c = o.available.unwrap();
        assert_eq!(c.tag, "v3.3.0", "v3.3.1 has no machine-learning image yet");
        assert_eq!(c.first_seen, 250, "seen once both were out");
    }

    #[test]
    fn blocked_units_offer_nothing() {
        let mut stack = jupiter();
        stack[3].image = "ghcr.io/goauthentik/server:2026.5.1".into();
        let units = units(&stack);
        let ak = unit(&units, "ghcr.io/goauthentik/server");
        assert_eq!(ak.blocked.as_deref(), Some("its services run different versions (2026.5.2, 2026.5.1)"));
        let o = offer(ak, &lists(&[("ghcr.io/goauthentik/server", &["2026.5.3"])]), &HashMap::new());
        assert!(o.available.is_none() && o.blocked.is_some());
    }

    #[test]
    fn the_stricter_label_wins() {
        let a = BTreeMap::from([("helion.update.hold".to_string(), "major".to_string())]);
        let b = BTreeMap::from([("helion.update.hold".to_string(), "minor".to_string())]);
        assert_eq!(merge_rules(&[&a, &b]).0.hold, Some(ChangeKind::Minor));
        assert_eq!(merge_rules(&[&b, &a]).0.hold, Some(ChangeKind::Minor));
        let d2 = BTreeMap::from([("helion.update.major-digits".to_string(), "2".to_string())]);
        let d1 = BTreeMap::from([("helion.update.major-digits".to_string(), "1".to_string())]);
        assert!(merge_rules(&[&d2, &d1]).1.is_some(), "no stricter side, so blocked");
    }
}
