//! Which icon a container gets: pure functions over its image, labels and
//! names, with no I/O. No fuzzy matching: a wrong icon is worse than the
//! generic one, and `helion.icon` fixes a miss in one line.

use super::index::{ valid_name, Index };

/// Names that are Sunstead's own. The app draws those itself, and an
/// upstream icon with the same name belongs to someone else's product.
const RESERVED: &[&str] = &["helion", "cosmos", "atlas", "solstice", "solstice-sync", "sunstead"];

/// Image-name endings that name a flavour of the same product, tried after
/// the whole name: `portainer-ce` is Portainer, `immich-server` is Immich.
const SUFFIXES: &[&str] = &["-server", "-app", "-ce", "-ee", "-web", "-ui", "-latest"];

/// What a container tells us about itself.
pub struct Subject<'a> {
    /// The `helion.icon` label: an icon name, or `none`.
    pub label: Option<&'a str>,
    pub service: Option<&'a str>,
    pub image: &'a str,
    pub compose_service: Option<&'a str>,
    pub name: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Match {
    Icon(String),
    /// `helion.icon=none`.
    OptedOut,
    Nothing,
}

pub fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// `ghcr.io/immich-app/immich-server:v1.2@sha256:...` -> `immich-server`.
pub fn image_name(image: &str) -> &str {
    let image = image.split('@').next().unwrap_or(image);
    let last = image.rsplit('/').next().unwrap_or(image);
    last.split(':').next().unwrap_or(last)
}

/// Lowercase, `_` and spaces as `-`.
pub fn normalize(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace(['_', ' '], "-")
}

/// The name itself, then each shorter form with a known ending removed.
fn forms(name: &str) -> Vec<String> {
    let mut out = vec![normalize(name)];
    for suffix in SUFFIXES {
        if let Some(stem) = out[0].strip_suffix(suffix) {
            if !stem.is_empty() {
                out.push(stem.to_string());
            }
        }
    }
    out.retain(|n| valid_name(n));
    out
}

pub fn find(index: &Index, subject: &Subject) -> Match {
    if let Some(label) = subject.label.map(normalize).filter(|l| !l.is_empty()) {
        if label == "none" {
            return Match::OptedOut;
        }
        if !is_reserved(&label) && index.contains(&label) {
            return Match::Icon(label);
        }
    }

    // The service key first: every container of a service then agrees on its
    // icon, so a database beside the app doesn't get the database's.
    let names = [
        subject.service,
        Some(image_name(subject.image)),
        subject.compose_service,
        Some(subject.name),
    ];
    for name in names.into_iter().flatten() {
        for form in forms(name) {
            if is_reserved(&form) {
                return Match::Nothing;
            }
            if index.contains(&form) {
                return Match::Icon(form);
            }
        }
    }
    Match::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(names: &[&str]) -> Index {
        let mut i = Index::default();
        for n in names {
            i.icons.insert((*n).into(), Default::default());
        }
        i
    }

    fn resolve(index: &Index, image: &str, name: &str) -> Match {
        find(index, &(Subject { label: None, service: None, image, compose_service: None, name }))
    }

    #[test]
    fn takes_the_repository_out_of_an_image_reference() {
        for (image, want) in [
            ("ghcr.io/immich-app/immich-server:v1.2", "immich-server"),
            ("lscr.io/linuxserver/nextcloud", "nextcloud"),
            ("louislam/uptime-kuma:1", "uptime-kuma"),
            ("docker.io/library/postgres:16-alpine", "postgres"),
            ("redis@sha256:abc", "redis"),
            ("localhost:5000/team/app:dev", "app"),
        ] {
            assert_eq!(image_name(image), want, "{image}");
        }
    }

    #[test]
    fn matches_real_images() {
        let i = index(&["immich", "nextcloud", "uptime-kuma", "postgres", "portainer", "gitea", "redis", "nginx"]);
        for (image, name, want) in [
            ("ghcr.io/immich-app/immich-server:v1", "immich_server", "immich"),
            ("lscr.io/linuxserver/nextcloud", "nc", "nextcloud"),
            ("louislam/uptime-kuma:1", "kuma", "uptime-kuma"),
            ("postgres:16", "db", "postgres"),
            ("portainer/portainer-ce:2", "portainer", "portainer"),
            ("gitea/gitea:1.22", "git", "gitea"),
            ("docker.io/library/nginx", "proxy", "nginx"),
        ] {
            assert_eq!(resolve(&i, image, name), Match::Icon(want.into()), "{image}");
        }
    }

    #[test]
    fn an_unknown_image_gets_nothing_not_a_guess() {
        let i = index(&["redis", "web"]);
        assert_eq!(resolve(&i, "ghcr.io/me/foo:1", "foo"), Match::Nothing);
        // `web` is an ending, not a product: stripping it must not match the icon `web`.
        assert_eq!(resolve(&i, "ghcr.io/me/redis-web", "x"), Match::Icon("redis".into()));
        assert_eq!(resolve(&i, "myapp-web", "x"), Match::Nothing);
    }

    #[test]
    fn a_service_key_beats_the_image() {
        let i = index(&["immich", "postgres"]);
        let subject = Subject {
            label: None,
            service: Some("immich"),
            image: "tensorchord/pgvecto-rs:pg14",
            compose_service: Some("database"),
            name: "immich_postgres",
        };
        assert_eq!(find(&i, &subject), Match::Icon("immich".into()));
    }

    #[test]
    fn the_label_overrides_and_can_opt_out() {
        let i = index(&["gitea", "forgejo"]);
        let mut subject = Subject { label: Some("Forgejo"), service: None, image: "gitea/gitea", compose_service: None, name: "git" };
        assert_eq!(find(&i, &subject), Match::Icon("forgejo".into()));
        subject.label = Some("none");
        assert_eq!(find(&i, &subject), Match::OptedOut);
        subject.label = Some("not-an-icon");
        assert_eq!(find(&i, &subject), Match::Icon("gitea".into()), "an unknown label falls back to matching");
    }

    #[test]
    fn sunstead_names_are_left_to_the_app() {
        let i = index(&["atlas", "helion", "solstice", "mongodb"]);
        assert_eq!(resolve(&i, "ghcr.io/sunstead/atlas:1", "atlas"), Match::Nothing);
        assert_eq!(resolve(&i, "ghcr.io/sunstead/helion-agent", "helion-agent"), Match::Nothing);
        let subject = Subject { label: Some("atlas"), service: None, image: "x", compose_service: None, name: "y" };
        assert_eq!(find(&i, &subject), Match::Nothing);
    }
}
