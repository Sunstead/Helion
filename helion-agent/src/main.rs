//! helion-agent — the per-node half of Helion.
//!
//! Design in one line: **sample on a schedule, never on the request path.**
//! Background samplers publish to `watch` channels; HTTP handlers hand out the
//! latest value, already serialized. A request does no collection, no
//! serialization and takes no locks.

mod api;
mod auth;
mod backups;
mod config;
mod docker;
mod error;
mod events;
mod history;
mod icons;
mod labels;
mod notify;
mod peers;
mod sample;
mod shutdown;
mod sse;
mod state;
mod store;
mod updates;
mod uptime;
mod wol;

use config::Config;
use docker::DockerHandle;
use sample::facts::HostFacts;
use state::{ AppState, Inner };
use std::{ process::ExitCode, sync::Arc, time::Duration };

/// How long open connections get to finish once the agent is asked to stop.
/// Docker waits 10 s before SIGKILL; this leaves room to flush history.
const DRAIN: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, path] = args.as_slice() {
        if flag == "--check-config" {
            return match Config::check_file(path) {
                Ok(()) => {
                    println!("helion-agent {}: {path} is valid", env!("CARGO_PKG_VERSION"));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("helion-agent {}: {e}", env!("CARGO_PKG_VERSION"));
                    ExitCode::FAILURE
                }
            };
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("helion-agent: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = Config::load()?;
    // Decided before anything is bound, so a misconfigured agent fails fast
    // rather than coming up open.
    let mode = cfg.auth_mode()?;
    cfg.peers_ready()?;
    match mode {
        config::AuthMode::Anonymous =>
            tracing::warn!(
                "running WITHOUT authentication (auth.allow_anonymous = true); \
                 anyone who can reach this port can read this node"
            ),
        config::AuthMode::Oidc { ignored_token: true } =>
            tracing::warn!("ignoring the old shared token; sign-in is through [auth.oidc] now"),
        config::AuthMode::Oidc { .. } => {}
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(serve(cfg, mode))
}

fn init_tracing() {
    use tracing_subscriber::{ fmt, prelude::*, EnvFilter };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_|
        EnvFilter::new("info,helion_agent=info")
    );
    // Colour only on a terminal; `docker logs` would show raw escape codes.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    tracing_subscriber
        ::registry()
        .with(fmt::layer().compact().with_ansi(ansi))
        .with(filter)
        .init();
}

async fn serve(cfg: Config, mode: config::AuthMode) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Arc::new(cfg);

    let facts = Arc::new(HostFacts::probe(cfg.node_name.as_deref()));
    tracing::info!(
        node = %facts.node_name,
        os = %facts.os,
        arch = %facts.arch,
        cores = facts.cpu_logical_cores,
        "helion-agent {} starting",
        env!("CARGO_PKG_VERSION")
    );

    // History is optional: a failure to open it must not stop the agent from
    // serving live metrics, which is its primary job.
    let history = if cfg.history.enabled {
        match history::HistoryHandle::open(&cfg.history) {
            Ok(h) => {
                tracing::info!(path = %cfg.history.path.display(), "metrics history enabled");
                Some(h)
            }
            Err(e) => {
                tracing::error!(
                    path = %cfg.history.path.display(),
                    error = %e,
                    hint = history::diagnose(&cfg.history.path),
                    "could not open metrics history; continuing without it"
                );
                None
            }
        }
    } else {
        None
    };

    let docker = DockerHandle::new(cfg.docker.socket.clone());

    let host_rx = sample::spawn_host(&cfg, facts.clone(), history.clone());
    let icons = cfg.icons.enabled.then(|| {
        let poke = docker.clone();
        icons::spawn(
            &cfg.icons,
            cfg.state.path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("icons"),
            move || poke.containers_changed().notify_one()
        )
    });
    let containers_rx = sample::spawn_containers(&cfg, docker.clone(), icons.clone());
    let volumes_rx = sample::spawn_volumes(docker.clone());

    sample::spawn_self_identification(docker.clone(), facts.hostname.clone());

    let (backups_rx, backups_poke) = match cfg.backups.enabled.then(|| sample::spawn_backups(&cfg)) {
        Some((rx, poke)) => (Some(rx), Some(poke)),
        None => (None, None),
    };
    let tailnet_rx = cfg.tailscale.enabled.then(|| sample::tailnet::spawn(&cfg.tailscale));

    // Like history, a broken state database disables the features that
    // need it rather than the agent.
    let store = if cfg.wol.enabled || cfg.events.enabled || cfg.uptime.enabled || cfg.updates.enabled {
        match store::Store::open(&cfg.state.path) {
            Ok(store) => Some(store),
            Err(e) => {
                tracing::error!(
                    path = %cfg.state.path.display(),
                    error = %e,
                    hint = history::diagnose(&cfg.state.path),
                    "could not open the state database; wake-on-lan, the event log and uptime checks are disabled"
                );
                None
            }
        }
    } else {
        None
    };

    let events = match (&store, cfg.events.enabled) {
        (Some(store), true) =>
            match events::spawn(&cfg.events, store.clone()).await {
                Ok(handle) => Some(handle),
                Err(e) => {
                    tracing::error!(error = %e, "could not start the event log");
                    None
                }
            }
        _ => None,
    };

    let (stop, shutdown) = shutdown::channel();

    // Its first report is a minute away at the soonest, after the notifier
    // below has subscribed.
    let peers = match (&cfg.peer_token, cfg.peers.is_empty()) {
        (Some(token), false) =>
            Some(
                peers::spawn(peers::Inputs {
                    peers: cfg.peers.clone(),
                    token: token.clone(),
                    node: facts.node_name.clone(),
                    events: events.clone(),
                    tailnet: tailnet_rx.clone(),
                    shutdown: shutdown.clone(),
                })
            ),
        _ => None,
    };

    // Before anything reports: the notifier only hears events stored after it
    // subscribes, and the lifecycle check below reports straight away.
    let notify = events
        .as_ref()
        .map(|events| notify::spawn(events.store.clone(), events, facts.node_name.clone(), peers.clone()));

    // The container tracker needs the Docker event stream even when the
    // samplers don't.
    let forward = events.as_ref().map(|events| {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        events::containers::spawn(events.clone(), rx, containers_rx.clone());
        tx
    });
    if cfg.docker.watch_events || forward.is_some() {
        sample::spawn_event_watcher(docker.clone(), cfg.docker.watch_events, forward);
    }

    if let Some(events) = &events {
        if let Err(e) = events::lifecycle::start(events, facts.node_name.clone()).await {
            tracing::warn!(error = %e, "cannot compare this run with the last");
        }
        events::disks::spawn(
            events.clone(),
            host_rx.clone(),
            cfg.events.disk_warn_pct,
            cfg.events.disk_critical_pct
        );
        if let Some(backups_rx) = &backups_rx {
            events::backups::spawn(events.clone(), backups_rx.clone());
        }
    }

    let wol = match (&store, cfg.wol.enabled) {
        (Some(store), true) => {
            let filters = sample::filters::HostFilters::from_config(&cfg.host);
            Some(
                sample::wol::spawn(&cfg.wol, store.clone(), tailnet_rx.clone(), events.clone(), move |iface|
                    filters.iface_allowed(iface)
                )
            )
        }
        _ => None,
    };

    let uptime = match (&store, cfg.uptime.enabled) {
        (Some(store), true) =>
            Some(
                uptime::spawn(
                    &cfg.uptime,
                    store.clone(),
                    containers_rx.clone(),
                    events.clone(),
                    cfg.peers.iter().map(|p| p.name.clone()).collect()
                )
            ),
        _ => None,
    };

    let updates = match (&store, cfg.updates.enabled) {
        (Some(store), true) =>
            Some(
                updates::spawn(&cfg.updates, updates::Inputs {
                    store: store.clone(),
                    docker: docker.clone(),
                    containers: containers_rx.clone(),
                    backups: backups_rx.clone(),
                    uptime: uptime.clone(),
                    events: events.clone(),
                })
            ),
        _ => None,
    };
    if cfg.updates.token.is_some() && cfg.updates.repo.is_none() {
        tracing::warn!("HELION_AGENT_GITHUB_TOKEN is set but [updates] repo isn't; updates are only listed");
    }

    let state = AppState::new(Inner {
        auth: match (mode, &cfg.auth.oidc) {
            (config::AuthMode::Oidc { .. }, Some(oidc)) => auth::Auth::oidc(oidc, cfg.auth.allow_query_token),
            _ => auth::Auth::anonymous(cfg.auth.allow_query_token),
        },
        facts,
        docker,
        history: history.clone(),
        host_rx,
        containers_rx,
        volumes_rx,
        backups_rx,
        backups_poke,
        tailnet_rx,
        wol,
        events: events.clone(),
        notify,
        uptime,
        updates,
        peers,
        icons,
        shutdown: shutdown.clone(),
        cfg: cfg.clone(),
    });

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(cfg.server.bind).await.map_err(|e| {
        format!("cannot bind {}: {e}", cfg.server.bind)
    })?;

    tracing::info!(
        addr = %cfg.server.bind,
        actions = cfg.allow_actions,
        "listening"
    );

    // Under Compose, a deploy or `docker compose down` sends SIGTERM.
    let store_for_stop = events.as_ref().map(|e| e.store.clone());
    tokio::spawn(async move {
        shutdown_signal().await;
        // Asked to stop, so the next start knows this wasn't a crash or a
        // power cut. Recorded first: if the drain below overruns Docker's
        // grace period, SIGKILL doesn't turn a deploy into a crash.
        if let Some(store) = &store_for_stop {
            events::lifecycle::stopped_cleanly(store).await;
        }
        // Ends the streams and log sockets, which would otherwise hold the
        // graceful shutdown open for as long as any app stays connected.
        stop.fire();
    });

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown.clone().wait());
    tokio::select! {
        result = server => result?,
        () = async {
            shutdown.wait().await;
            tokio::time::sleep(DRAIN).await;
        } => tracing::warn!("connections still open after {}s; stopping anyway", DRAIN.as_secs()),
    }

    if let Some(history) = history {
        let _ = tokio::task::spawn_blocking(move || history.flush(Duration::from_secs(2))).await;
    }

    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received interrupt, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
