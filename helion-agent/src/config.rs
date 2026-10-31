//! Agent configuration: a TOML file with a `HELION_AGENT_*` environment
//! overlay on top. Everything has a working default except auth: the agent
//! refuses to start without an identity provider unless explicitly opened
//! up with `allow_anonymous`.
//!
//! Any string in the file can name environment variables, `${NAME}` (`$$`
//! for a dollar sign), so one file serves any domain: a deployment keeps
//! its names in `.env` and passes them to the container. A variable that
//! isn't set, or is empty, is an error.

use serde::Deserialize;
use std::{ net::SocketAddr, path::{ Path, PathBuf } };

/// Where the databases live unless the config says otherwise.
const DATA_DIR: &str = "/var/lib/helion-agent";
/// Where 0.13 and older kept them.
const LEGACY_DATA_DIR: &str = "/var/lib/cosmos-agent";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")] Read {
        path: String,
        #[source] source: std::io::Error,
    },
    #[error("parsing {path}: {source}")] Parse {
        path: String,
        #[source] source: toml::de::Error,
    },
    #[error("{0}")] Invalid(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Defaults to the system hostname.
    pub node_name: Option<String>,
    /// What this node is for, in a few words ("Public edge"), shown in the
    /// app beside its name.
    pub node_description: Option<String>,
    /// Off by default: a node is read-only until you say otherwise. Gates
    /// every mutating route (container and volume actions, Wake-on-LAN), and
    /// even then only for admins.
    pub allow_actions: bool,
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub cors: CorsConfig,
    pub host: HostConfig,
    pub docker: DockerConfig,
    pub history: HistoryConfig,
    pub backups: BackupsConfig,
    pub web: WebConfig,
    pub state: StateConfig,
    pub tailscale: TailscaleConfig,
    pub wol: WolConfig,
    pub events: EventsConfig,
    pub uptime: UptimeConfig,
    pub updates: UpdatesConfig,
    pub icons: IconsConfig,
    /// Other agents this one exchanges heartbeats with, each reporting the
    /// other when it stops answering (`[[peers]]`).
    pub peers: Vec<PeerConfig>,
    /// From `HELION_AGENT_PEER_TOKEN` only, never the file: the secret every
    /// peer shares. Needed when `peers` is set.
    #[serde(skip)]
    pub peer_token: Option<crate::peers::PeerToken>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    /// The peer's node name, which it sends in its heartbeats.
    pub name: String,
    /// Where to reach its agent. Set: this agent sends it heartbeats. Unset:
    /// it sends them here. Only one side needs to reach the other.
    pub url: Option<String>,
    /// Its web UI, so this agent's web UI lists it too. Defaults to `url`.
    pub ui_url: Option<String>,
    /// Its machine name on the tailnet, to say whether the tailnet sees it
    /// online when it stops answering. Defaults to `name`.
    pub tailnet_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventsConfig {
    /// The event log: what happened on this node, and the problems open now.
    /// Kept in `[state] path`.
    pub enabled: bool,
    /// Events older than this are deleted, except one that opened a problem
    /// still open.
    pub retain_days: u32,
    /// A filesystem this full opens a warning, and this full an error. Each
    /// clears 5 points below.
    pub disk_warn_pct: u8,
    pub disk_critical_pct: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Checks registries for newer tags of the images running here. Read
    /// only, and anonymous; applying one needs `repo` and a token.
    pub enabled: bool,
    pub check_interval_hours: u32,
    /// A tag must have been seen this long before it applies on its own.
    pub min_age_days: u32,
    /// `owner/name` of the repository whose workflow applies an update: it
    /// bumps the tag in compose, commits and deploys. Unset: updates are only
    /// listed.
    pub repo: Option<String>,
    pub workflow: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// How long after a deploy a failing check or restarts count as the
    /// update's fault.
    pub watch_minutes: u32,
    /// From `HELION_AGENT_GITHUB_TOKEN` only, never the file: Actions read and
    /// write on `repo`, nothing else.
    #[serde(skip)]
    pub token: Option<crate::updates::github::Secret>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IconsConfig {
    /// Finds an icon for each service from homarr-labs/dashboard-icons and
    /// keeps the files here. The only outbound requests are for the index and
    /// for icons that match something running.
    pub enabled: bool,
    /// Where `metadata.json` and `svg/<name>.svg` are fetched from. Point it
    /// at a mirror on a network without internet, or at a pinned ref
    /// (`.../dashboard-icons@<commit>`).
    pub source: String,
    /// How old the index may get before it is fetched again.
    pub refresh_days: u32,
}

impl Default for IconsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            source: "https://cdn.jsdelivr.net/gh/homarr-labs/dashboard-icons".into(),
            refresh_days: 7,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UptimeConfig {
    /// HTTP checks for every service with a `helion.service.url` label, plus
    /// the checks added in the UI (kept in `[state] path`).
    pub enabled: bool,
    /// How often each check runs, unless a check says otherwise.
    pub interval_secs: u32,
    /// Names under these domains are checked through 127.0.0.1, i.e. the
    /// reverse proxy on this host, instead of what DNS says. For a proxy that
    /// serves names resolving to an address this host can't reach itself,
    /// such as its own Tailscale IP.
    pub local_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WolConfig {
    /// Targets are added in the UI and kept in `[state] path`. Waking and
    /// editing also need `allow_actions` and an admin.
    pub enabled: bool,
    pub interval_ms: u64,
    /// How long after a packet to keep watching before calling it failed.
    pub wake_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TailscaleConfig {
    pub enabled: bool,
    /// The host's `tailscaled` socket, bind-mounted in. Read-only access is
    /// all `tailscaled` gives a non-root caller, and all the agent needs.
    pub socket: PathBuf,
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateConfig {
    /// Settings edited from the UI. Separate from the metrics history,
    /// whose writer thread owns that database.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    /// A built web UI (`app/dist`) to serve from the agent's own origin.
    /// The Docker image sets this; unset means API only.
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub shutdown_grace_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Sign-in through an OpenID Connect provider (Authentik).
    pub oidc: Option<OidcConfig>,
    /// Removed in 0.3. Still parsed so an old config fails with an
    /// explanation instead of "unknown field".
    pub token: Option<String>,
    pub token_file: Option<PathBuf>,
    /// Browser `EventSource` and `WebSocket` cannot set request headers, so
    /// the streaming routes need `?token=`. Turning this off breaks live
    /// metrics in the web build.
    pub allow_query_token: bool,
    /// Must be set explicitly to run without sign-in. Guards against
    /// accidentally exposing container actions to the whole LAN.
    pub allow_anonymous: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// Exactly as the provider states it, e.g.
    /// `https://auth.example.com/application/o/helion/`.
    pub issuer: String,
    /// The app's client ID; tokens must be addressed to it.
    pub client_id: String,
    /// Defaults to `client_id`.
    #[serde(default)]
    pub audience: Option<String>,
    /// Defaults to the issuer's `/.well-known/openid-configuration`. Set it
    /// when the agent must reach the provider by a different address.
    #[serde(default)]
    pub discovery_url: Option<String>,
    /// Members of any of these may act (with `allow_actions`); everyone
    /// else who can sign in is a viewer.
    #[serde(default = "default_admin_groups")]
    pub admin_groups: Vec<String>,
    /// What the app asks for. `offline_access` gets it a refresh token.
    #[serde(default = "default_scopes")]
    pub scopes: String,
}

fn default_admin_groups() -> Vec<String> {
    vec!["homelab-admins".into()]
}

fn default_scopes() -> String {
    "openid profile email offline_access".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    pub extra_origins: Vec<String>,
    /// Some WKWebView configurations send `Origin: null` for custom-scheme
    /// pages. Off by default because it's a broad allowance.
    pub allow_null_origin: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
    pub interval_ms: u64,
    /// Glob patterns. Ignored when `net_include` is non-empty.
    pub net_exclude: Vec<String>,
    /// Non-empty turns this into an allowlist.
    pub net_include: Vec<String>,
    /// Which filesystems to report, and what to call them. Empty means
    /// "report everything with a non-zero size", which is right on a host and
    /// wrong in a container (you get overlayfs and every bind mount).
    pub disks: Vec<DiskConfig>,
    /// Where the host's `/` is mounted in the container, e.g. `/host/rootfs`.
    /// Every real filesystem under it (the bind is recursive) is reported
    /// under its host path, so a second drive needs no entry of its own.
    /// Defaults to the `disks` entry labelled `/`.
    pub root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskConfig {
    /// Mount point as the agent sees it, e.g. `/host/rootfs`.
    pub path: String,
    /// What to show the user, e.g. `/`. Defaults to `path`.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerConfig {
    pub socket: String,
    pub interval_ms: u64,
    /// Cap on simultaneous `stats` calls against the Docker socket.
    pub stats_concurrency: usize,
    /// Deprecated: use the top-level `allow_actions`, which now also covers
    /// Wake-on-LAN. Still read so existing configs keep working.
    pub allow_actions: Option<bool>,
    pub allow_logs: bool,
    /// Subscribe to the Docker event stream so the container list refreshes
    /// immediately on start/stop rather than at the next poll.
    pub watch_events: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub flush_interval_secs: u64,
    pub retain_1s_secs: i64,
    pub retain_1m_secs: i64,
    pub retain_5m_secs: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackupsConfig {
    pub enabled: bool,
    /// Written by the backup job on the host. The agent only reads it.
    pub status_file: PathBuf,
    /// systemd timer stamp file. Its mtime is an independent signal that the
    /// timer fired, even if the job died before writing a status.
    pub timer_stamp: Option<PathBuf>,
    /// Used to decide whether the status has gone stale.
    pub expected_interval_secs: u64,
    /// Display name. Never the real repository path.
    pub repo_label: String,
    /// The host's request inbox, mounted writable. With it (and
    /// `allow_actions`), admins can start a backup or a restore test: the agent
    /// drops a small file here and the host does the rest.
    pub requests_dir: Option<PathBuf>,
    /// Where the host writes each request's result, one JSON file per request.
    pub request_results_dir: Option<PathBuf>,
    /// The latest restore test's result, written by the host.
    pub restore_test_file: Option<PathBuf>,
}

// --- defaults --------------------------------------------------------------

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 7700)),
            shutdown_grace_secs: 10,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            oidc: None,
            token: None,
            token_file: None,
            allow_query_token: true,
            allow_anonymous: false,
        }
    }
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            // Under `network_mode: host` the agent sees every bridge and veth
            // the host has. Summing those into "node throughput" double-counts
            // container traffic.
            // `lo*` rather than `lo`: Linux names loopback `lo`, macOS `lo0`.
            // Loopback is the single biggest distorter of "node throughput".
            net_exclude: ["lo*", "docker*", "br-*", "veth*", "virbr*", "cni*", "flannel*"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            net_include: Vec::new(),
            disks: Vec::new(),
            root: None,
        }
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            socket: "/var/run/docker.sock".to_string(),
            interval_ms: 2000,
            stats_concurrency: 8,
            allow_actions: None,
            allow_logs: true,
            watch_events: true,
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: Path::new(DATA_DIR).join("metrics.db"),
            flush_interval_secs: 10,
            retain_1s_secs: 3_600, // 1 hour
            retain_1m_secs: 604_800, // 7 days
            retain_5m_secs: 7_776_000, // 90 days
        }
    }
}

impl Default for TailscaleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket: PathBuf::from("/var/run/tailscale/tailscaled.sock"),
            interval_ms: 10_000,
        }
    }
}

impl Default for WolConfig {
    fn default() -> Self {
        Self { enabled: false, interval_ms: 10_000, wake_timeout_secs: 180 }
    }
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self { enabled: true, retain_days: 180, disk_warn_pct: 85, disk_critical_pct: 95 }
    }
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_hours: 6,
            min_age_days: 3,
            repo: None,
            workflow: "update.yml".into(),
            git_ref: "main".into(),
            watch_minutes: 10,
            token: None,
        }
    }
}

impl Default for UptimeConfig {
    fn default() -> Self {
        Self { enabled: true, interval_secs: 60, local_domains: Vec::new() }
    }
}

impl Default for StateConfig {
    fn default() -> Self {
        Self { path: Path::new(DATA_DIR).join("state.db") }
    }
}

impl Default for BackupsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            status_file: PathBuf::from("/host/backups/restic-status.json"),
            timer_stamp: None,
            expected_interval_secs: 86_400,
            repo_label: "restic".to_string(),
            requests_dir: None,
            request_results_dir: None,
            restore_test_file: None,
        }
    }
}

// --- loading ---------------------------------------------------------------

impl Config {
    /// Reads the TOML file named by `HELION_AGENT_CONFIG` (if set and present),
    /// then applies the environment overlay, then validates.
    pub fn load() -> Result<Self, ConfigError> {
        let get = |name: &str| std::env::var(name).ok();
        let mut cfg = match env(&get, "HELION_AGENT_CONFIG") {
            Some(path) => Self::from_file(&path)?,
            // No config file is a perfectly good way to run the agent: the
            // defaults plus the environment are enough.
            None => Self::default(),
        };
        cfg.migrate_deprecated()?;
        cfg.apply_env(&get)?;
        cfg.use_legacy_data_dir(|p| p.exists());
        cfg.validate()?;
        Ok(cfg)
    }

    /// Folds `docker.allow_actions` into the top-level switch. Setting both
    /// to different values is an error rather than a silent precedence rule.
    fn migrate_deprecated(&mut self) -> Result<(), ConfigError> {
        if let Some(v) = self.docker.allow_actions.take() {
            if self.allow_actions && !v {
                return Err(
                    ConfigError::Invalid(
                        "allow_actions and docker.allow_actions disagree; remove docker.allow_actions".into()
                    )
                );
            }
            tracing::warn!(
                "docker.allow_actions is deprecated; move it to the top-level allow_actions"
            );
            self.allow_actions |= v;
        }
        Ok(())
    }

    /// Whether this version accepts the config at `path`, without the
    /// `HELION_AGENT_*` overlay. For `--check-config`: an update workflow runs
    /// the new image against the live config before switching to it. The
    /// file's `${NAME}`s are still expanded, so the check needs them set.
    pub fn check_file(path: &str) -> Result<(), ConfigError> {
        let mut cfg = Self::from_file(path)?;
        cfg.migrate_deprecated()?;
        cfg.validate()?;
        cfg.auth_mode()?;
        Ok(())
    }

    fn from_file(path: &str) -> Result<Self, ConfigError> {
        let text = std::fs
            ::read_to_string(path)
            .map_err(|source| ConfigError::Read { path: path.to_string(), source })?;
        parse(&text, path, |name| std::env::var(name).ok())
    }

    /// The environment overlay. `get` is the process environment in
    /// production and a map in tests.
    fn apply_env(&mut self, get: Getter) -> Result<(), ConfigError> {
        if let Some(v) = env(get, "HELION_NODE_NAME") {
            self.node_name = Some(v);
        }
        if let Some(v) = env(get, "HELION_AGENT_BIND") {
            self.server.bind = v
                .parse()
                .map_err(|_| ConfigError::Invalid(format!("HELION_AGENT_BIND is not an address: {v}")))?;
        }
        // Only to reject a pre-0.3 setup with an explanation (see `auth_mode`).
        if let Some(v) = get("COSMOS_AGENT_TOKEN") {
            self.auth.token = Some(v);
        }
        if let Some(v) = get("COSMOS_AGENT_TOKEN_FILE") {
            self.auth.token_file = Some(PathBuf::from(v));
        }
        if let Some(issuer) = env(get, "HELION_AGENT_OIDC_ISSUER") {
            let client_id = env(get, "HELION_AGENT_OIDC_CLIENT_ID").unwrap_or_else(|| "helion".into());
            let oidc = self.auth.oidc.get_or_insert_with(|| OidcConfig {
                issuer: issuer.clone(),
                client_id: client_id.clone(),
                audience: None,
                discovery_url: None,
                admin_groups: default_admin_groups(),
                scopes: default_scopes(),
            });
            oidc.issuer = issuer;
            oidc.client_id = client_id;
        }
        if let Some(v) = env_bool(get, "HELION_AGENT_ALLOW_ACTIONS")? {
            self.allow_actions = v;
        }
        if let Some(v) = env_bool(get, "HELION_AGENT_ALLOW_ANONYMOUS")? {
            self.auth.allow_anonymous = v;
        }
        if let Some(v) = env(get, "HELION_AGENT_DOCKER_SOCKET") {
            self.docker.socket = v;
        }
        if let Some(v) = env_bool(get, "HELION_AGENT_HISTORY")? {
            self.history.enabled = v;
        }
        if let Some(v) = env(get, "HELION_AGENT_HISTORY_PATH") {
            self.history.path = PathBuf::from(v);
        }
        if let Some(v) = env(get, "HELION_AGENT_GITHUB_TOKEN") {
            let v = v.trim().to_string();
            self.updates.token = (!v.is_empty()).then_some(crate::updates::github::Secret(v));
        }
        if let Some(v) = env(get, "HELION_AGENT_PEER_TOKEN") {
            let v = v.trim().to_string();
            self.peer_token = (!v.is_empty()).then_some(crate::peers::PeerToken(v));
        }
        if let Some(v) = env(get, "HELION_AGENT_WEB_DIR") {
            self.web.dir = (!v.is_empty()).then(|| PathBuf::from(v));
        }
        Ok(())
    }

    /// 0.13 and older kept their databases in `/var/lib/cosmos-agent`. The image
    /// has both directories, so a deployment that still mounts its volume at
    /// the old path would otherwise start on an empty history in the new one.
    /// Only the default locations move, and only when the old database is
    /// there and the new one isn't.
    fn use_legacy_data_dir(&mut self, exists: impl Fn(&Path) -> bool) {
        for (path, file) in [(&mut self.history.path, "metrics.db"), (&mut self.state.path, "state.db")] {
            let current = Path::new(DATA_DIR).join(file);
            let legacy = Path::new(LEGACY_DATA_DIR).join(file);
            if *path == current && !exists(&current) && exists(&legacy) {
                tracing::warn!(
                    "{} is where the data used to live; using it. Move it to {} or set the path in the config",
                    legacy.display(),
                    current.display()
                );
                *path = legacy;
            }
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.host.interval_ms < 200 {
            return Err(
                ConfigError::Invalid(
                    "host.interval_ms below 200 samples faster than the CPU counters update".into()
                )
            );
        }
        if self.docker.interval_ms < 500 {
            return Err(ConfigError::Invalid("docker.interval_ms must be at least 500".into()));
        }
        if self.tailscale.interval_ms < 1000 {
            return Err(ConfigError::Invalid("tailscale.interval_ms must be at least 1000".into()));
        }
        if self.wol.interval_ms < 1000 {
            return Err(ConfigError::Invalid("wol.interval_ms must be at least 1000".into()));
        }
        if self.wol.wake_timeout_secs < 10 {
            return Err(ConfigError::Invalid("wol.wake_timeout_secs must be at least 10".into()));
        }
        if self.docker.stats_concurrency == 0 {
            return Err(ConfigError::Invalid("docker.stats_concurrency must be at least 1".into()));
        }
        if self.events.retain_days == 0 {
            return Err(ConfigError::Invalid("events.retain_days must be at least 1".into()));
        }
        if self.updates.check_interval_hours == 0 || self.updates.check_interval_hours > 168 {
            return Err(ConfigError::Invalid("updates.check_interval_hours must be between 1 and 168".into()));
        }
        if !(1..=60).contains(&self.updates.watch_minutes) {
            return Err(ConfigError::Invalid("updates.watch_minutes must be between 1 and 60".into()));
        }
        if let Some(repo) = &self.updates.repo {
            let ok = repo
                .split_once('/')
                .is_some_and(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'));
            if !ok {
                return Err(ConfigError::Invalid(format!("updates.repo is owner/name, not {repo}")));
            }
        }
        if !(20..=3_600).contains(&self.uptime.interval_secs) {
            return Err(ConfigError::Invalid("uptime.interval_secs must be between 20 and 3600".into()));
        }
        if self.uptime.local_domains.iter().any(|d| d.trim_matches('.').is_empty()) {
            return Err(ConfigError::Invalid("uptime.local_domains has an empty entry".into()));
        }
        self.validate_peers()?;
        let (warn, critical) = (self.events.disk_warn_pct, self.events.disk_critical_pct);
        if !(10..critical).contains(&warn) || critical > 100 {
            return Err(
                ConfigError::Invalid(
                    "events.disk_warn_pct must be at least 10 and below events.disk_critical_pct, which is at most 100".into()
                )
            );
        }
        Ok(())
    }

    /// Names are unique and plain, and URLs are web addresses. The token is
    /// checked at startup (`peers_ready`), not here: `--check-config` reads a
    /// file without the environment.
    fn validate_peers(&self) -> Result<(), ConfigError> {
        let mut seen = Vec::new();
        for p in &self.peers {
            let ok = !p.name.is_empty() &&
                p.name.len() <= 64 &&
                p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !ok {
                return Err(ConfigError::Invalid(format!("peer name {:?} must be letters, digits, - or _", p.name)));
            }
            if seen.contains(&&p.name) {
                return Err(ConfigError::Invalid(format!("peer {} is listed twice", p.name)));
            }
            seen.push(&p.name);
            for url in [&p.url, &p.ui_url].into_iter().flatten() {
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(ConfigError::Invalid(format!("peer {}: {url} must start with http:// or https://", p.name)));
                }
            }
        }
        Ok(())
    }

    /// Peers need the shared token; without one they're off, loudly.
    pub fn peers_ready(&self) -> Result<(), ConfigError> {
        match &self.peer_token {
            _ if self.peers.is_empty() => Ok(()),
            Some(t) if t.0.len() >= 32 => Ok(()),
            Some(_) => Err(ConfigError::Invalid("HELION_AGENT_PEER_TOKEN must be at least 32 characters".into())),
            None => Err(ConfigError::Invalid("[[peers]] is set but HELION_AGENT_PEER_TOKEN isn't".into())),
        }
    }

    /// How callers authenticate. An agent that can stop containers must
    /// never come up open by accident, so no provider and no explicit
    /// `allow_anonymous` is a startup failure.
    pub fn auth_mode(&self) -> Result<AuthMode, ConfigError> {
        let legacy = self.auth.token.as_deref().is_some_and(|t| !t.trim().is_empty()) ||
            self.auth.token_file.is_some();
        match (&self.auth.oidc, self.auth.allow_anonymous) {
            (Some(oidc), _) => {
                if oidc.issuer.trim().is_empty() || oidc.client_id.trim().is_empty() {
                    return Err(ConfigError::Invalid("auth.oidc needs an issuer and a client_id".into()));
                }
                Ok(AuthMode::Oidc { ignored_token: legacy })
            }
            (None, true) => Ok(AuthMode::Anonymous),
            (None, false) if legacy =>
                Err(
                    ConfigError::Invalid(
                        "the shared agent token was removed in 0.3. Configure [auth.oidc] with \
                         your identity provider (see agent.example.toml), and remove \
                         COSMOS_AGENT_TOKEN / auth.token".into()
                    )
                ),
            (None, false) =>
                Err(
                    ConfigError::Invalid(
                        "no sign-in configured. Set [auth.oidc] (or HELION_AGENT_OIDC_ISSUER), or \
                         auth.allow_anonymous = true if this agent really should be open to anyone \
                         who can reach it".into()
                    )
                ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthMode {
    Anonymous,
    /// `ignored_token`: an old shared token is still configured.
    Oidc {
        ignored_token: bool,
    },
}

type Getter<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Reads a `HELION_…` variable, or failing that the same name under the
/// `COSMOS_` prefix that 0.13 and older used, warning that the old one is
/// deprecated. The new name wins when both are set.
fn env(get: Getter, name: &str) -> Option<String> {
    if let Some(v) = get(name) {
        return Some(v);
    }
    let legacy = name.replacen("HELION_", "COSMOS_", 1);
    let v = get(&legacy)?;
    tracing::warn!("{legacy} is deprecated; rename it to {name}");
    Some(v)
}

fn env_bool(get: Getter, key: &str) -> Result<Option<bool>, ConfigError> {
    match env(get, key) {
        None => Ok(None),
        Some(v) =>
            match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(Some(true)),
                "0" | "false" | "no" | "off" => Ok(Some(false)),
                other => Err(ConfigError::Invalid(format!("{key} is not a boolean: {other}"))),
            }
    }
}

/// Parses a config file, expanding `${NAME}` in its strings through `lookup`.
/// Strings are expanded after parsing, so TOML quoting is never in play; a
/// file without a `$` is read directly, which keeps line numbers in errors.
fn parse(
    text: &str,
    path: &str,
    lookup: impl Fn(&str) -> Option<String>
) -> Result<Config, ConfigError> {
    let parse_err = |source| ConfigError::Parse { path: path.to_string(), source };
    if !text.contains('$') {
        return toml::from_str(text).map_err(parse_err);
    }
    let mut value = toml::Value::Table(toml::from_str(text).map_err(parse_err)?);
    expand_value(&mut value, String::new(), &lookup)?;
    value.try_into().map_err(parse_err)
}

fn expand_value(
    value: &mut toml::Value,
    key: String,
    lookup: &impl Fn(&str) -> Option<String>
) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(s) => {
            *s = expand(s, &key, lookup)?;
        }
        toml::Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                expand_value(item, format!("{key}[{i}]"), lookup)?;
            }
        }
        toml::Value::Table(table) => {
            for (k, item) in table.iter_mut() {
                let key = if key.is_empty() { k.clone() } else { format!("{key}.{k}") };
                expand_value(item, key, lookup)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `${NAME}` replaced by its value, `$$` by `$`; any other `$` is kept.
fn expand(
    s: &str,
    key: &str,
    lookup: &impl Fn(&str) -> Option<String>
) -> Result<String, ConfigError> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(body) = after.strip_prefix('{') {
            let end = body
                .find('}')
                .ok_or_else(|| ConfigError::Invalid(format!("{key}: an unclosed ${{ in {s:?}")))?;
            let name = &body[..end];
            let valid =
                name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') &&
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                return Err(ConfigError::Invalid(format!("{key}: ${{{name}}} isn't a variable name")));
            }
            match lookup(name) {
                Some(v) if !v.is_empty() => out.push_str(&v),
                _ => {
                    return Err(
                        ConfigError::Invalid(
                            format!("{key} uses ${{{name}}}, which isn't set in the agent's environment")
                        )
                    );
                }
            }
            rest = &body[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    mod env_vars {
        use super::super::*;

        fn vars(name: &str) -> Option<String> {
            match name {
                "HOME_DOMAIN" => Some("home.example.com".into()),
                "TAILNET" => Some("tail1234.ts.net".into()),
                "EMPTY" => Some(String::new()),
                _ => None,
            }
        }

        #[test]
        fn expands_strings_in_nested_tables_and_arrays() {
            let text = r#"
                [auth.oidc]
                issuer = "https://auth.${HOME_DOMAIN}/application/o/cosmos/"
                client_id = "cosmos"
                [uptime]
                local_domains = ["${HOME_DOMAIN}", "example.org"]
                [[peers]]
                name = "pluto"
                ui_url = "https://pluto.${TAILNET}:7700"
            "#;
            let cfg = parse(text, "t.toml", vars).unwrap();
            assert_eq!(
                cfg.auth.oidc.unwrap().issuer,
                "https://auth.home.example.com/application/o/cosmos/"
            );
            assert_eq!(cfg.uptime.local_domains, ["home.example.com", "example.org"]);
            assert_eq!(cfg.peers[0].ui_url.as_deref(), Some("https://pluto.tail1234.ts.net:7700"));
        }

        #[test]
        fn names_the_key_and_the_variable_that_is_missing() {
            for missing in ["${NOPE}", "${EMPTY}"] {
                let text = format!("[[peers]]\nname = \"jupiter\"\nurl = \"https://{missing}\"");
                let err = parse(&text, "t.toml", vars).unwrap_err().to_string();
                assert!(err.contains("peers[0].url"), "{err}");
                assert!(err.contains(missing), "{err}");
            }
        }

        #[test]
        fn a_double_dollar_is_a_dollar_and_a_lone_one_stays() {
            assert_eq!(expand("a$$b", "k", &vars).unwrap(), "a$b");
            assert_eq!(expand("$${HOME_DOMAIN}", "k", &vars).unwrap(), "${HOME_DOMAIN}");
            assert_eq!(expand("cost $5", "k", &vars).unwrap(), "cost $5");
            assert!(expand("${HOME_DOMAIN", "k", &vars).is_err());
            assert!(expand("${1X}", "k", &vars).is_err());
        }

        #[test]
        fn keys_are_never_expanded() {
            let err = parse("[\"${HOME_DOMAIN}\"]\nx = 1", "t.toml", vars).unwrap_err().to_string();
            assert!(err.contains("${HOME_DOMAIN}"), "an unknown table, by its literal name: {err}");
        }

        #[test]
        fn a_file_without_variables_parses_directly() {
            let cfg = parse("node_name = \"jupiter\"", "t.toml", |_| None).unwrap();
            assert_eq!(cfg.node_name.as_deref(), Some("jupiter"));
        }
    }

    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().expect("shipped defaults must validate");
    }

    #[test]
    fn no_sign_in_is_a_startup_error_unless_anonymous_is_explicit() {
        let mut cfg = Config::default();
        assert!(cfg.auth_mode().is_err(), "an agent with no sign-in must refuse to start");

        cfg.auth.allow_anonymous = true;
        assert_eq!(cfg.auth_mode().unwrap(), AuthMode::Anonymous);
    }

    #[test]
    fn an_old_token_config_explains_what_to_do() {
        let mut cfg = Config::default();
        cfg.auth.token = Some("s3cret".into());
        let err = cfg.auth_mode().unwrap_err().to_string();
        assert!(err.contains("[auth.oidc]"), "{err}");
    }

    #[test]
    fn parses_peers_and_needs_their_token() {
        let mut cfg: Config = toml
            ::from_str(
                r#"
                node_description = "Public edge"

                [[peers]]
                name = "jupiter"
                url = "https://helion.jupiter.example.net"

                [[peers]]
                name = "saturn"
                ui_url = "https://saturn.example.net:7700"
            "#
            )
            .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.peers.len(), 2);
        assert_eq!(cfg.node_description.as_deref(), Some("Public edge"));
        assert!(cfg.peers_ready().is_err(), "no token");
        cfg.peer_token = Some(crate::peers::PeerToken("short".into()));
        assert!(cfg.peers_ready().is_err(), "too short");
        cfg.peer_token = Some(crate::peers::PeerToken("x".repeat(32)));
        cfg.peers_ready().unwrap();
        assert!(!format!("{cfg:?}").contains(&"x".repeat(32)));
    }

    #[test]
    fn refuses_bad_peers() {
        for bad in [
            "[[peers]]\nname = \"has space\"",
            "[[peers]]\nname = \"a\"\n[[peers]]\nname = \"a\"",
            "[[peers]]\nname = \"a\"\nurl = \"helion.example.net\"",
        ] {
            let cfg: Config = toml::from_str(bad).unwrap();
            assert!(cfg.validate().is_err(), "{bad}");
        }
        Config::default().peers_ready().unwrap();
    }

    #[test]
    fn parses_oidc_with_defaults() {
        let cfg: Config = toml
            ::from_str(
                r#"
                [auth.oidc]
                issuer = "https://auth.example.com/application/o/cosmos/"
                client_id = "cosmos"
            "#
            )
            .unwrap();
        let oidc = cfg.auth.oidc.as_ref().unwrap();
        assert_eq!(oidc.admin_groups, ["homelab-admins"]);
        assert!(oidc.scopes.contains("offline_access"));
        assert_eq!(cfg.auth_mode().unwrap(), AuthMode::Oidc { ignored_token: false });
    }

    #[test]
    fn rejects_sampling_faster_than_the_cpu_counters_update() {
        let mut cfg = Config::default();
        cfg.host.interval_ms = 50;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn parses_the_host_root() {
        let cfg: Config = toml::from_str("[host]\nroot = \"/hostfs\"").expect("valid config");
        assert_eq!(cfg.host.root.as_deref(), Some("/hostfs"));
        assert_eq!(Config::default().host.root, None);
    }

    #[test]
    fn parses_a_config_file() {
        let cfg: Config = toml
            ::from_str(
                r#"
                node_name = "jupiter"
                allow_actions = true
                [server]
                bind = "127.0.0.1:9000"
                [[host.disks]]
                path = "/host/rootfs"
                label = "/"
            "#
            )
            .expect("valid config");

        assert_eq!(cfg.node_name.as_deref(), Some("jupiter"));
        assert_eq!(cfg.server.bind.port(), 9000);
        assert!(cfg.allow_actions);
        assert_eq!(cfg.host.disks.len(), 1);
        assert_eq!(cfg.host.disks[0].label.as_deref(), Some("/"));
        // Untouched sections keep their defaults.
        assert_eq!(cfg.host.interval_ms, 1000);
        assert!(cfg.events.enabled);
        assert_eq!(cfg.events.retain_days, 180);
    }

    #[test]
    fn disk_thresholds_must_be_in_order() {
        let parse = |s: &str| toml::from_str::<Config>(s).unwrap();
        assert!(parse("[events]\ndisk_warn_pct = 80\ndisk_critical_pct = 90").validate().is_ok());
        assert!(parse("[events]\ndisk_warn_pct = 95\ndisk_critical_pct = 90").validate().is_err());
        assert!(parse("[events]\ndisk_critical_pct = 101").validate().is_err());
        assert!(parse("[events]\nretain_days = 0").validate().is_err());
    }

    #[test]
    fn the_example_config_is_valid() {
        let cfg: Config = toml::from_str(include_str!("../agent.example.toml")).expect("the example parses");
        cfg.validate().expect("the example validates");
    }

    #[test]
    fn parses_update_settings_and_hides_the_token() {
        let parse = |s: &str| toml::from_str::<Config>(s).unwrap();
        let mut cfg = parse("[updates]\nrepo = \"example/homelab\"\nref = \"main\"\nmin_age_days = 5");
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.updates.repo.as_deref(), Some("example/homelab"));
        assert_eq!(cfg.updates.min_age_days, 5);
        assert!(parse("[updates]\nrepo = \"Jupiter\"").validate().is_err());
        assert!(toml::from_str::<Config>("[updates]\ntoken = \"x\"").is_err(), "never from the file");

        cfg.updates.token = Some(crate::updates::github::Secret("github_pat_SECRET".into()));
        assert!(!format!("{cfg:?}").contains("SECRET"));
    }

    #[test]
    fn parses_uptime_settings() {
        let parse = |s: &str| toml::from_str::<Config>(s).unwrap();
        let cfg = parse("[uptime]\ninterval_secs = 30\nlocal_domains = [\"jupiter.example.net\"]");
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.uptime.local_domains, ["jupiter.example.net"]);
        assert!(cfg.uptime.enabled);
        assert!(parse("[uptime]\ninterval_secs = 5").validate().is_err());
        assert!(parse("[uptime]\nlocal_domains = [\"\"]").validate().is_err());
    }

    #[test]
    fn deprecated_docker_allow_actions_still_enables_actions() {
        let mut cfg: Config = toml::from_str("[docker]\nallow_actions = true").unwrap();
        cfg.migrate_deprecated().unwrap();
        assert!(cfg.allow_actions);
        assert_eq!(cfg.docker.allow_actions, None);
    }

    #[test]
    fn conflicting_allow_actions_is_an_error() {
        let mut cfg: Config = toml
            ::from_str("allow_actions = true\n[docker]\nallow_actions = false")
            .unwrap();
        assert!(cfg.migrate_deprecated().is_err());
    }

    /// The overlay with only the given variables set.
    fn overlay(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let get = |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
        let mut cfg = Config::default();
        cfg.apply_env(&get)?;
        Ok(cfg)
    }

    #[test]
    fn the_environment_overlay_takes_helion_names() {
        let cfg = overlay(
            &[
                ("HELION_NODE_NAME", "pluto"),
                ("HELION_AGENT_BIND", "127.0.0.1:9000"),
                ("HELION_AGENT_ALLOW_ACTIONS", "yes"),
                ("HELION_AGENT_HISTORY_PATH", "/tmp/h.db"),
                ("HELION_AGENT_WEB_DIR", "/web"),
            ]
        ).unwrap();
        assert_eq!(cfg.node_name.as_deref(), Some("pluto"));
        assert_eq!(cfg.server.bind.to_string(), "127.0.0.1:9000");
        assert!(cfg.allow_actions);
        assert_eq!(cfg.history.path, PathBuf::from("/tmp/h.db"));
        assert_eq!(cfg.web.dir, Some(PathBuf::from("/web")));
    }

    #[test]
    fn the_environment_overlay_still_takes_cosmos_names() {
        let cfg = overlay(
            &[
                ("COSMOS_NODE_NAME", "jupiter"),
                ("COSMOS_AGENT_BIND", "127.0.0.1:9001"),
                ("COSMOS_AGENT_OIDC_ISSUER", "https://auth.example/application/o/cosmos/"),
                ("COSMOS_AGENT_OIDC_CLIENT_ID", "cosmos"),
                ("COSMOS_AGENT_ALLOW_ANONYMOUS", "true"),
                ("COSMOS_AGENT_HISTORY", "off"),
                ("COSMOS_AGENT_HISTORY_PATH", "/tmp/old.db"),
                ("COSMOS_AGENT_GITHUB_TOKEN", " ghp_x "),
                ("COSMOS_AGENT_PEER_TOKEN", &"p".repeat(32)),
            ]
        ).unwrap();
        assert_eq!(cfg.node_name.as_deref(), Some("jupiter"));
        assert_eq!(cfg.server.bind.to_string(), "127.0.0.1:9001");
        let oidc = cfg.auth.oidc.expect("issuer from the old name");
        assert_eq!((oidc.issuer.as_str(), oidc.client_id.as_str()), ("https://auth.example/application/o/cosmos/", "cosmos"));
        assert!(cfg.auth.allow_anonymous);
        assert!(!cfg.history.enabled, "HISTORY is not mistaken for HISTORY_PATH");
        assert_eq!(cfg.history.path, PathBuf::from("/tmp/old.db"));
        assert_eq!(cfg.updates.token.map(|t| t.0), Some("ghp_x".to_string()));
        assert_eq!(cfg.peer_token.map(|t| t.0.len()), Some(32));
    }

    #[test]
    fn the_new_name_wins_when_both_are_set() {
        let cfg = overlay(&[("COSMOS_AGENT_DOCKER_SOCKET", "/old.sock"), ("HELION_AGENT_DOCKER_SOCKET", "/new.sock")]).unwrap();
        assert_eq!(cfg.docker.socket, "/new.sock");
        let cfg = overlay(&[("COSMOS_AGENT_ALLOW_ACTIONS", "true"), ("HELION_AGENT_ALLOW_ACTIONS", "false")]).unwrap();
        assert!(!cfg.allow_actions);
    }

    #[test]
    fn a_bad_legacy_value_names_the_current_variable() {
        let err = overlay(&[("COSMOS_AGENT_ALLOW_ACTIONS", "maybe")]).unwrap_err().to_string();
        assert!(err.contains("HELION_AGENT_ALLOW_ACTIONS"), "{err}");
    }

    #[test]
    fn there_is_no_helion_agent_token() {
        let cfg = overlay(&[("HELION_AGENT_TOKEN", "s3cret")]).unwrap();
        assert_eq!(cfg.auth.token, None);
        let cfg = overlay(&[("COSMOS_AGENT_TOKEN", "s3cret")]).unwrap();
        assert!(cfg.auth_mode().unwrap_err().to_string().contains("COSMOS_AGENT_TOKEN"));
    }

    #[test]
    fn a_new_install_defaults_to_the_helion_client_id() {
        let cfg = overlay(&[("HELION_AGENT_OIDC_ISSUER", "https://auth.example/application/o/helion/")]).unwrap();
        assert_eq!(cfg.auth.oidc.unwrap().client_id, "helion");
    }

    #[test]
    fn peers_need_the_token_under_either_name() {
        let peers = r#"
            [[peers]]
            name = "jupiter"
            url = "https://helion.example.com"
        "#;
        let long = "t".repeat(32);
        for (vars, ok) in [
            (vec![], false),
            (vec![("HELION_AGENT_PEER_TOKEN", long.as_str())], true),
            (vec![("COSMOS_AGENT_PEER_TOKEN", long.as_str())], true),
            (vec![("COSMOS_AGENT_PEER_TOKEN", "short")], false),
            (vec![("HELION_AGENT_PEER_TOKEN", ""), ("COSMOS_AGENT_PEER_TOKEN", "")], false),
        ] {
            let mut cfg: Config = toml::from_str(peers).unwrap();
            let get = |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
            cfg.apply_env(&get).unwrap();
            assert_eq!(cfg.peers_ready().is_ok(), ok, "{vars:?}");
        }
        let err = Config { peers: toml::from_str::<Config>(peers).unwrap().peers, ..Config::default() }
            .peers_ready()
            .unwrap_err()
            .to_string();
        assert!(err.contains("HELION_AGENT_PEER_TOKEN"), "{err}");
    }

    #[test]
    fn data_stays_in_the_old_directory_until_the_new_one_has_a_database() {
        let both = |_: &Path| true;
        let neither = |_: &Path| false;
        let only_old = |p: &Path| p.starts_with(LEGACY_DATA_DIR);

        let mut cfg = Config::default();
        cfg.use_legacy_data_dir(only_old);
        assert_eq!(cfg.history.path, PathBuf::from("/var/lib/cosmos-agent/metrics.db"));
        assert_eq!(cfg.state.path, PathBuf::from("/var/lib/cosmos-agent/state.db"));

        for exists in [&both as &dyn Fn(&Path) -> bool, &neither] {
            let mut cfg = Config::default();
            cfg.use_legacy_data_dir(exists);
            assert_eq!(cfg.history.path, PathBuf::from("/var/lib/helion-agent/metrics.db"));
            assert_eq!(cfg.state.path, PathBuf::from("/var/lib/helion-agent/state.db"));
        }

        let mut cfg = Config::default();
        cfg.history.path = PathBuf::from("/data/custom.db");
        cfg.use_legacy_data_dir(only_old);
        assert_eq!(cfg.history.path, PathBuf::from("/data/custom.db"), "an explicit path is left alone");
        assert_eq!(cfg.state.path, PathBuf::from("/var/lib/cosmos-agent/state.db"));
    }
}
