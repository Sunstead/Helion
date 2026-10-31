//! Service icons, found without anyone choosing them.
//!
//! The sampler asks [`IconsHandle::resolve`] for each container; the answer is
//! a map lookup against an index of homarr-labs/dashboard-icons (kept on disk,
//! refreshed in the background), and an icon is reported only once its file is
//! here, so the app never asks for one that isn't. A missing file is fetched
//! by a background task, which then pokes the sampler so the name appears on
//! the next list. The app reads the files back through `GET /v1/icons/:name`.
//!
//! Nothing here is on the request path, and an unreachable source only means
//! no new icons: what is cached keeps serving.

pub mod index;
pub mod ink;
pub mod matcher;

use crate::config::IconsConfig;
use index::{ valid_name, Index };
use ink::{ classify, Background };
use matcher::{ find, Match, Subject };
use std::{
    collections::{ HashMap, HashSet },
    path::PathBuf,
    sync::{ Arc, Mutex, RwLock },
    time::{ Duration, Instant, SystemTime },
};
use tokio::sync::mpsc;

/// Larger than any icon in the source; a bigger answer is something else.
const MAX_SVG_BYTES: usize = 512 * 1024;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
/// After a failed fetch of one icon, how long before trying it again.
const RETRY: Duration = Duration::from_secs(3600);

#[derive(Clone)]
pub struct IconsHandle(Arc<Inner>);

struct Inner {
    dir: PathBuf,
    source: String,
    refresh: Duration,
    http: reqwest::Client,
    index: RwLock<Arc<Index>>,
    /// Icons whose files are on disk.
    ready: RwLock<HashSet<String>>,
    /// Queued or being fetched.
    queued: Mutex<HashSet<String>>,
    failed: Mutex<HashMap<String, Instant>>,
    /// Containers already reported as having no icon, so the log says it once.
    unmatched: Mutex<HashSet<String>>,
    fetch: mpsc::UnboundedSender<String>,
    changed: Box<dyn Fn() + Send + Sync>,
}

/// Starts the background tasks. `dir` is where the index and files live;
/// `changed` runs whenever the answers `resolve` gives may have changed.
pub fn spawn(
    cfg: &IconsConfig,
    dir: PathBuf,
    changed: impl Fn() + Send + Sync + 'static
) -> IconsHandle {
    let (fetch, queue) = mpsc::unbounded_channel();
    let http = reqwest::Client
        ::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("helion-agent/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let handle = IconsHandle(
        Arc::new(Inner {
            dir,
            source: cfg.source.trim_end_matches('/').to_string(),
            refresh: Duration::from_secs(u64::from(cfg.refresh_days.max(1)) * 86_400),
            http,
            index: RwLock::new(Arc::new(Index::default())),
            ready: RwLock::new(HashSet::new()),
            queued: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashMap::new()),
            unmatched: Mutex::new(HashSet::new()),
            fetch,
            changed: Box::new(changed),
        })
    );
    handle.0.load_ready();
    tokio::spawn(refresh_index(handle.clone()));
    tokio::spawn(fetch_icons(handle.clone(), queue));
    handle
}

impl IconsHandle {
    /// The icon for a container, if one is known and already here.
    pub fn resolve(&self, subject: &Subject) -> Option<String> {
        let index = self.0.index.read().ok()?.clone();
        if index.is_empty() {
            return None;
        }
        match find(&index, subject) {
            Match::Icon(name) => {
                if self.0.ready.read().ok()?.contains(&name) {
                    Some(name)
                } else {
                    self.0.queue(name);
                    None
                }
            }
            Match::OptedOut => None,
            Match::Nothing => {
                let first = self.0.unmatched.lock().is_ok_and(|mut s| s.insert(subject.name.to_string()));
                if first {
                    tracing::info!(
                        container = subject.name,
                        image = subject.image,
                        "no icon for this container; set the helion.icon label to name one"
                    );
                }
                None
            }
        }
    }

    /// The file to show `name` on `background`: the icon, or the variant of it
    /// whose ink reads there.
    pub async fn svg(&self, name: &str, background: Background) -> Option<Vec<u8>> {
        if !valid_name(name) || !self.0.ready.read().ok()?.contains(name) {
            return None;
        }
        let variants = self.0.index.read().ok()?.icons.get(name).map(|e| e.variants.clone()).unwrap_or_default();
        let mut files = Vec::new();
        for file in std::iter::once(name.to_string()).chain(variants) {
            if let Ok(bytes) = tokio::fs::read(self.0.path(&file)).await {
                let ink = classify(&String::from_utf8_lossy(&bytes));
                files.push((file, ink, bytes));
            }
        }
        let choices: Vec<_> = files.iter().map(|(f, i, _)| (f.clone(), *i)).collect();
        let chosen = ink::pick(&choices, background)?.to_string();
        files.into_iter().find(|(f, _, _)| *f == chosen).map(|(_, _, bytes)| bytes)
    }
}

impl Inner {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join("svg").join(format!("{name}.svg"))
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn load_ready(&self) {
        let Ok(entries) = std::fs::read_dir(self.dir.join("svg")) else {
            return;
        };
        let names = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".svg")).map(String::from))
            .filter(|n| valid_name(n));
        if let Ok(mut ready) = self.ready.write() {
            ready.extend(names);
        }
    }

    fn queue(&self, name: String) {
        if self.failed.lock().is_ok_and(|f| f.get(&name).is_some_and(|at| at.elapsed() < RETRY)) {
            return;
        }
        if self.queued.lock().is_ok_and(|mut q| q.insert(name.clone())) {
            let _ = self.fetch.send(name);
        }
    }

    async fn get(&self, url: String, limit: usize) -> Result<Vec<u8>, String> {
        let response = self.http.get(&url).send().await.map_err(|e| format!("{url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{url}: {}", response.status()));
        }
        let bytes = response.bytes().await.map_err(|e| format!("{url}: {e}"))?;
        if bytes.len() > limit {
            return Err(format!("{url}: {} bytes is more than an icon source sends", bytes.len()));
        }
        Ok(bytes.to_vec())
    }

    async fn write(&self, path: PathBuf, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await
    }

    async fn fetch_icon(&self, name: &str) -> Result<(), String> {
        let variants = self.index
            .read()
            .map_err(|_| "index lock".to_string())?
            .icons.get(name)
            .map(|e| e.variants.clone())
            .ok_or_else(|| format!("{name} is not in the index"))?;

        // Variants first, so the icon's own file being here means they were
        // tried. A variant that fails just isn't offered.
        for variant in variants {
            if let Ok(bytes) = self.svg_from_source(&variant).await {
                self.write(self.path(&variant), &bytes).await.map_err(|e| e.to_string())?;
            }
        }
        let bytes = self.svg_from_source(name).await?;
        self.write(self.path(name), &bytes).await.map_err(|e| e.to_string())
    }

    async fn svg_from_source(&self, name: &str) -> Result<Vec<u8>, String> {
        let bytes = self.get(format!("{}/svg/{name}.svg", self.source), MAX_SVG_BYTES).await?;
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        if !text.contains("<svg") || text.contains("<script") {
            return Err(format!("{name}: not a plain svg"));
        }
        Ok(bytes)
    }
}

/// Keeps the index: the copy on disk at once, then a fresh one whenever it is
/// older than `refresh_days`.
async fn refresh_index(handle: IconsHandle) {
    let inner = &handle.0;
    if let Ok(bytes) = tokio::fs::read(inner.index_path()).await {
        if let Ok(index) = serde_json::from_slice::<Index>(&bytes).map(Index::finish) {
            tracing::debug!(icons = index.icons.len(), "icon index loaded from disk");
            if let Ok(mut slot) = inner.index.write() {
                *slot = Arc::new(index);
            }
            (inner.changed)();
        }
    }

    loop {
        let age = tokio::fs
            ::metadata(inner.index_path()).await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        if age.is_none_or(|a| a >= inner.refresh) {
            let fetched = inner.get(format!("{}/metadata.json", inner.source), MAX_INDEX_BYTES).await;
            match fetched.and_then(|b| Index::parse(&b).map_err(|e| e.to_string())) {
                Ok(index) if !index.is_empty() => {
                    match serde_json::to_vec(&index) {
                        Ok(json) => {
                            if let Err(e) = inner.write(inner.index_path(), &json).await {
                                tracing::warn!(error = %e, "could not save the icon index");
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "could not save the icon index"),
                    }
                    tracing::info!(icons = index.icons.len(), "icon index refreshed");
                    if let Ok(mut slot) = inner.index.write() {
                        *slot = Arc::new(index);
                    }
                    (inner.changed)();
                }
                Ok(_) => tracing::warn!("the icon source's index is empty; keeping the old one"),
                Err(e) => tracing::warn!(error = %e, "could not refresh the icon index; will retry"),
            }
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn fetch_icons(handle: IconsHandle, mut queue: mpsc::UnboundedReceiver<String>) {
    let inner = &handle.0;
    while let Some(name) = queue.recv().await {
        match inner.fetch_icon(&name).await {
            Ok(()) => {
                if let Ok(mut ready) = inner.ready.write() {
                    ready.insert(name.clone());
                }
                (inner.changed)();
            }
            Err(e) => {
                tracing::warn!(icon = %name, error = %e, "could not fetch an icon; will retry later");
                if let Ok(mut failed) = inner.failed.lock() {
                    failed.insert(name.clone(), Instant::now());
                }
            }
        }
        if let Ok(mut queued) = inner.queued.lock() {
            queued.remove(&name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{ routing::get, Router };
    use std::sync::atomic::{ AtomicUsize, Ordering };
    use tokio::sync::Notify;

    const METADATA: &str =
        r#"{
        "gitea": {"base": "svg"},
        "github": {"base": "svg", "colors": {"dark": "github", "light": "github-light"}}
    }"#;

    async fn source() -> String {
        let app = Router::new()
            .route("/metadata.json", get(|| async { METADATA }))
            .route("/svg/gitea.svg", get(|| async { r##"<svg viewBox="0 0 1 1"><path fill="#609926"/></svg>"## }))
            .route("/svg/github.svg", get(|| async { r##"<svg><path fill="#1b1f23"/></svg>"## }))
            .route("/svg/github-light.svg", get(|| async { r##"<svg><path fill="#fff"/></svg>"## }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn subject<'a>(image: &'a str, name: &'a str) -> Subject<'a> {
        Subject { label: None, service: None, image, compose_service: None, name }
    }

    async fn settle(poke: &Notify, handle: &IconsHandle, subject: &Subject<'_>) -> Option<String> {
        for _ in 0..20 {
            if let Some(found) = handle.resolve(subject) {
                return Some(found);
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), poke.notified()).await;
        }
        None
    }

    #[tokio::test]
    async fn fetches_an_icon_for_a_container_and_serves_the_variant_that_reads() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = IconsConfig { source: source().await, ..IconsConfig::default() };
        let poke = Arc::new(Notify::new());
        let pokes = Arc::new(AtomicUsize::new(0));
        let handle = {
            let (poke, pokes) = (poke.clone(), pokes.clone());
            spawn(&cfg, dir.path().into(), move || {
                pokes.fetch_add(1, Ordering::SeqCst);
                poke.notify_one();
            })
        };

        let found = settle(&poke, &handle, &subject("gitea/gitea:1.22", "git")).await;
        assert_eq!(found.as_deref(), Some("gitea"));
        assert!(pokes.load(Ordering::SeqCst) >= 2, "told the sampler of the index and of the file");
        assert!(handle.svg("gitea", Background::Dark).await.is_some());

        let github = settle(&poke, &handle, &subject("ghcr.io/x/github", "gh")).await;
        assert_eq!(github.as_deref(), Some("github"));
        let on_light = String::from_utf8(handle.svg("github", Background::Light).await.unwrap()).unwrap();
        let on_dark = String::from_utf8(handle.svg("github", Background::Dark).await.unwrap()).unwrap();
        assert!(on_light.contains("#1b1f23") && on_dark.contains("#fff"));

        assert!(handle.svg("../etc/passwd", Background::Dark).await.is_none());
        assert!(handle.resolve(&subject("ghcr.io/me/unknown", "u")).is_none());
    }

    #[tokio::test]
    async fn an_unreachable_source_leaves_the_cache_serving() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("svg")).unwrap();
        std::fs::write(dir.path().join("svg/gitea.svg"), r##"<svg><path fill="#609926"/></svg>"##).unwrap();
        std::fs::write(
            dir.path().join("index.json"),
            serde_json::to_vec(&Index::parse(METADATA.as_bytes()).unwrap()).unwrap()
        ).unwrap();

        let cfg = IconsConfig { source: "http://127.0.0.1:9".into(), ..IconsConfig::default() };
        let poke = Arc::new(Notify::new());
        let handle = {
            let poke = poke.clone();
            spawn(&cfg, dir.path().into(), move || poke.notify_one())
        };
        let found = settle(&poke, &handle, &subject("gitea/gitea", "git")).await;
        assert_eq!(found.as_deref(), Some("gitea"));
    }

    /// Against the real source: `cargo test -p helion-agent real_source -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn real_source_matches_common_homelab_images() {
        let dir = tempfile::tempdir().unwrap();
        let poke = Arc::new(Notify::new());
        let handle = {
            let poke = poke.clone();
            spawn(&IconsConfig::default(), dir.path().into(), move || poke.notify_one())
        };
        for (image, name, want) in [
            ("ghcr.io/immich-app/immich-server:release", "immich_server", "immich"),
            ("lscr.io/linuxserver/jellyfin", "jellyfin", "jellyfin"),
            ("grafana/grafana-oss:11", "grafana", "grafana"),
            ("vaultwarden/server:latest", "vaultwarden", "vaultwarden"),
            ("portainer/portainer-ce:2", "portainer", "portainer"),
            ("louislam/uptime-kuma:1", "kuma", "uptime-kuma"),
            ("ghcr.io/x/opencloud", "opencloud", "open-cloud"),
            ("ghcr.io/goauthentik/server:2026.5", "authentik", "authentik"),
            ("tailscale/tailscale", "tailscale", "tailscale"),
            ("containrrr/watchtower", "watchtower", "watchtower"),
        ] {
            let found = settle(&poke, &handle, &subject(image, name)).await;
            assert_eq!(found.as_deref(), Some(want), "{image}");
            for bg in [Background::Dark, Background::Light] {
                assert!(handle.svg(want, bg).await.is_some(), "{want}");
            }
        }
    }
}
