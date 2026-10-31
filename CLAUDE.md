# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Helion is a homelab dashboard. A Cargo workspace holds three crates, plus a React frontend inside the Tauri crate's parent directory:

- `helion-agent` — axum server that runs on each node (port 7700). Host metrics (sysinfo), Docker containers/volumes (bollard), metrics history (SQLite), restic backup status.
- `helion-common` — shared serde types, exported to TypeScript via `ts-rs`.
- `app/src-tauri` — Tauri 2 shell around the frontend; also targets Android (`gen/android`).
- `app/` — Vite + React 19 + TypeScript (Tailwind v4, `@sunstead/ui` primitives on Base UI, TanStack Router/Query/Table, zustand, recharts).

## Commands

Rust (from repo root):
```bash
cargo test -p helion-common                      # ALSO regenerates the TS bindings
cargo test --workspace
cargo clippy --workspace --all-targets           # expected to be warning-free
HELION_AGENT_ALLOW_ANONYMOUS=true HELION_AGENT_BIND=127.0.0.1:7700 cargo run -p helion-agent   # dev: no sign-in
cargo build --profile release-agent -p helion-agent
```

Frontend (from `app/`):
```bash
npm run dev                        # Vite on :1420 (browser build)
npm run tauri dev                  # desktop app
npm run build                      # tsc && vite build
npm run lint                       # 0 errors, 0 warnings; ui/ is vendored and exempt from the copy rule
npm test                           # vitest + jsdom + Testing Library
npx vitest run src/lib/lib.test.ts # single file
npm run test:e2e                   # Playwright, see below
npm run release -- 0.6.0 [--push]  # agent, web UI and desktop app together; see RELEASING.md
```

`npm run test:e2e` builds and starts a real `helion-agent` (random port, temp
history DB, a fake `tailscaled` and a mock OIDC provider in `e2e/mock-oidc.ts`,
so every spec signs in for real) and Vite on :1431, then drives system Chrome
(`PW_CHANNEL` overrides). Docker-dependent specs skip when no daemon is up.
Screenshots of every page in Sunstead Dark and Sunstead Light, and of Overview in
every theme (`theme-<id>-overview.png`), land in `app/e2e/.results/`.
`e2e/constellation.html` (dev server only, never built) renders the
constellation on fixture data, 5 nodes, 50 services and 15 devices, for
judging its look and frame time without Docker.

Agent container (agent + bundled web UI):
```bash
docker build -f helion-agent/Dockerfile -t helion-agent .   # build from the REPO ROOT
```
`.github/workflows/agent-image.yml` publishes `ghcr.io/sunstead/helion-agent`
(linux/amd64): `:latest` from `main`, semver tags from `v*`. `ci.yml` runs the
agent/common tests, clippy (`-D warnings`) and the frontend lint/test/build;
the Tauri crate is skipped in CI because it needs WebKitGTK. `app-release.yml`
builds the desktop app (universal macOS dmg, Windows msi/exe) on `app-v*` tags
into a draft release. **One version covers the agent, web UI and desktop app**:
`app/package.json` (which `tauri.conf.json` references and Settings shows),
the Tauri, agent and common `Cargo.toml`s, all set by `app/scripts/release.mjs`,
which tags `v<version>` (agent image) and `app-v<version>` (desktop) together.

## Type sharing (Rust → TS)

Types live in `helion-common/src/types/` (`host`, `docker`, `history`, `backups`, `meta`, `error`), flat-re-exported from `types/mod.rs`. Each derives `ts_rs::TS` with `#[ts(export, export_to = "../../app/src/generated/")]`.

Two things to know:

- **`export_to` resolves against `helion-common/bindings/`, not the source file.** That's why every module uses the identical path string, and why the types can be reorganised freely.
- **64-bit integers carry `#[ts(type = "number")]`.** ts-rs maps `u64`/`i64` to `bigint`, but serde writes plain JSON numbers and `JSON.parse` yields `number` — without the annotation the generated type never matches the value at runtime. Every such field here is a byte count, timestamp or counter, all far below `Number.MAX_SAFE_INTEGER`.

`app/src/generated/` is gitignored, so **a fresh clone has no types until you run `cargo test -p helion-common`** — `npm run build` fails until you do.

**Units on the wire are raw: bytes and bytes/second.** Formatting belongs in `app/src/lib/node-metrics.ts`. The pre-0.2 format mixed MB/s for disk with megabits/s for network in one struct.

## Agent architecture

**The one rule: sample on a schedule, never on the request path.**

Background samplers publish to `tokio::sync::watch` channels; handlers hand out the latest value, already serialized. A request does no collection, no serde and takes no locks — `/v1/host` is ~0.3 ms.

- `sample/host.rs` — `HostProbe` owns persistent `System`/`Disks`/`Networks`, refreshed in place, on a dedicated OS thread (sysinfo does blocking `/proc` reads, and the loop owns `&mut System` for its lifetime). It exclusively owns `prev_net`/`prev_disk`, which is why rate deltas are correct with any number of clients. Absolute-deadline scheduling avoids drift and catch-up bursts. The one `MINIMUM_CPU_UPDATE_INTERVAL` sleep happens once at startup. On Linux, disk I/O doesn't come from sysinfo (`sample/diskstats.rs`): sysinfo resolves `/dev/<device>` to find a disk's counters, and the distroless container has no host block devices, so every disk read 0 B/s. Instead each mount's `major:minor` from `/proc/self/mountinfo` (re-read on re-enumeration) is looked up in `/proc/diskstats` (read every tick; it isn't namespaced, so it shows the host's devices), falling back to the source's name under `/dev` for `0:N` mounts like btrfs. A disk with no counters is sent with `io_available: false`, and the app and history totals leave it out. Jupiter's mountinfo and diskstats are test fixtures.
- `sample/docker.rs` — `DockerProbe` computes container CPU % by differencing absolute counters across *our own* ticks (`one_shot: true` zeroes `precpu_stats`, so differencing against those is meaningless). `started_at` is cached against state transitions, so steady state costs zero inspect calls. `stats` calls are bounded by `buffer_unordered`.
- `sample/mod.rs` — spawns everything. Host 1 s, containers 2 s, volumes 60 s, backups 30 s. The Docker event watcher and the action handlers poke each sampler's own `Notify` (volume events and container create/destroy wake volumes too, since `in_use_by` changes) so lists refresh immediately. Pokes use `notify_one`, which stores a permit: `notify_waiters` lost any poke that landed mid-sample. Containers and volumes both stream (`/v1/volumes/stream`, `volume_stream` capability); older agents' volumes are polled every 30 s.
- `watch` not `broadcast`: subscribers only want the newest sample, and `watch` coalesces by construction.
- Snapshots carry `json: Arc<str>` — serialization happens once per tick, not once per request per client.

**Config** (`config.rs`). `agent.toml`, then the `HELION_AGENT_*` overlay.
Any string in the file may name environment variables, `${NAME}` (`$$` is a
dollar), expanded after parsing so TOML quoting never matters; unset or empty
is a startup error naming the key. That's how a deployment keeps its domains
in `.env`: nothing in this repo or the infrastructure repos names a real
domain. `--check-config` expands too, so it needs the same variables.

**Stopping** (`main.rs`, `shutdown.rs`). axum's graceful shutdown waits for every response to finish, and SSE streams and log sockets never do, so on SIGTERM the agent first records the clean stop (so an overrun can't make a deploy read as a crash), then fires a `Shutdown` signal that ends streams (`sse::stream_watch` takes it) and log sockets (a `closed` frame saying the agent is restarting), drains for at most 5 s, and flushes the history writer explicitly (the sampler thread holds a handle forever, so its channel never closes). Any new long-lived response must end on `state.shutdown` too, or one open app keeps the agent alive until Docker's SIGKILL.

**Routes.** `/healthz` and `/v1/info` are public; everything else needs a token; every mutating route sits behind `require_write`, which needs both the node's top-level `allow_actions` and an admin `Principal` (and logs one `action` line saying who). `docker.allow_actions` is a deprecated alias. `/v1/logs` and `/v1/logs/ws` (`all_logs` capability) merge every running container: the socket keeps each container's Docker stream in a `StreamMap`, adds containers that start later from the container snapshot's `running` IDs, and tags lines with `container`; the client orders the interleaved backlogs by timestamp (`compareLogTime`: Docker trims trailing fraction zeros, so the strings don't sort). Logs use a WebSocket (`/v1/containers/:id/logs/ws`) because Docker log frames contain embedded newlines, `EventSource` reconnects uncontrollably against crash-looping containers, and browsers cap ~6 HTTP/1.1 connections per origin.

`/v1/info` is unauthenticated so the add-node flow can distinguish "nothing here" from "needs a token", but `node_name` and `principal` stay `null` until authenticated. The app narrows capabilities by `principal.admin` in `effectiveCapabilities` (`api/client.ts`), so UI code only ever checks capabilities. A **404 there means a pre-0.2 agent** → assume `api_version: 0`.

**Auth** (`auth/`). Sign-in is OpenID Connect through Authentik; the shared token is gone (an old `auth.token`/`COSMOS_AGENT_TOKEN` config fails to start with an explanation). The app gets an access token (a JWT, ~10 min) and sends it as a bearer token, or as `?token=` for `EventSource`/`WebSocket`, which cannot set headers; the trace layer records `uri.path()` only so query tokens never reach the logs. `auth/oidc.rs` verifies offline against the provider's JWKS (asymmetric algorithms only, `iss`, `aud` = client ID, `exp` with 60 s leeway), caching keys by `kid` and refetching an unknown `kid` at most once a minute. An unreachable provider is **503, not 401**, so the app retries instead of re-prompting. `groups` ∩ `admin_groups` makes an admin; everyone else who can sign in is a viewer. Streams are checked when they open, not cut at expiry. `/v1/info` publishes the non-secret `auth` block (`issuer`, `client_id`, `scopes`) so the app can start sign-in before it has a token. **CORS must stay the outermost layer** — axum applies layers bottom-up, and an unauthenticated `OPTIONS` preflight that 401s without CORS headers surfaces as an opaque browser failure. The agent refuses to start with no provider unless `auth.allow_anonymous = true`.

**Sign-in, app side.** `stores/auth.ts` keeps one session per issuer (every node trusting it shares it), access tokens in memory, refreshed a minute before expiry with one refresh in flight per issuer. The browser build runs PKCE itself (`lib/oidc.ts`, `/auth/callback`) and keeps the refresh token in `localStorage`; Authentik only sends CORS headers to *strict* redirect URIs, which is why the web one is strict. The desktop app does the whole flow in Rust (`src-tauri/src/oidc.rs`, loopback redirect on a random port) so the refresh token goes from the provider to the keychain and never reaches the webview. `NodeConnection` takes a token provider, re-reads `/v1/info` with the token to learn the principal, refreshes once on a 401, and otherwise sits in `unauthorized` until a session for that issuer appears, when the node store retries it. **`null` from the token provider means "sign in"; a rejection (`ProviderUnreachable`) means the provider is down**, and the connection retries like any outage: a deploy restarting Authentik must not park every node on a sign-in prompt. A removed session (sign-out, revoked refresh token) re-checks that issuer's nodes at once. `lib/accounts.ts` derives one `Account` per provider (session, role, waiting nodes) for the sidebar's account menu and Settings; the status toasts say a sign-in expired only for nodes that had worked this session, once per provider, and never after a deliberate sign-out.

**Errors** (`error.rs`). Every handler returns `Result<T, AgentError>` rendering `ApiError { code, message, detail }`. The `not_enabled` (501, hide the feature) vs `unavailable`/`docker_unavailable` (503, retry) split is load-bearing for the UI. `/v1/containers` returns 503 when Docker is down — never an empty list — and the container stream sends the last known list with `docker_unavailable: true` (once per outage, same `sampled_at`, so the detectors see no change), which the app shows as a note rather than "No containers".

**History** (`history/`). SQLite, WAL + `synchronous = NORMAL` + `temp_store = MEMORY` (the last is required for `read_only: true` containers). Tiers: `metrics_1s` (1 h), `metrics_1m` (7 d), `metrics_5m` (90 d), each rolled-up tier carrying avg *and* max. A writer thread batches one transaction per 60 rows or 10 s; the sampler's `push` is `try_send` and drops rather than stalling the live stream. **Rollups only touch fully-closed buckets**, which makes them idempotent and crash-safe via a cursor in `meta`; retention never deletes rows the rollup hasn't consumed. Because a bucket is never revisited, the writer flushes its batch before rolling up and waits 5 s past the close for rows in flight. The 5m tier weights each minute by its sample count `n`. `/v1/metrics` returns columnar parallel arrays (~4× smaller than row objects at 10k points).

**Tailnet** (`sample/tailnet.rs`). Polls `tailscaled`'s LocalAPI (`/localapi/v0/status`) over its unix socket every 10 s: raw HTTP/1.0, so the body ends at EOF and no HTTP client is needed. `tailscaled` authorises by peer uid and gives non-root, non-operator callers read-only access, so no API key and no write path. The snapshot keeps the parsed `TailnetStatus` as well as the JSON because Wake-on-LAN reads `online` from it. A device is matched to a Helion node when that node's agent reports it as `is_self`, never by hostname (`lib/tailnet.ts` `mergeTailnets`). A device's `name` is its machine name, the first label of its MagicDNS name as `tailscale status` shows it, not the OS hostname, which every iPhone and iPad reports as `localhost`; `mergeTailnets` derives it too, for 0.8.0 and older agents. The UI polls rather than streams; it's on the Network page and as the constellation's outer ring of probes.

**Wake-on-LAN** (`wol.rs`, `sample/wol.rs`, `store.rs`). Targets are edited in the UI and stored in `state.db` (`[state] path`), a separate SQLite file from the history database, with `PRAGMA user_version` migrations that are append-only. One task owns the target list and in-flight wakes; handlers read its snapshot, or send it a command (`reload`, `waking`) and wait for it to republish, so a GET right after a write reflects it. "Up" comes from the linked tailnet device's `online`, else a TCP probe where a refusal counts as up. The state machine (`step`) is pure and tested with a fake clock; each wake's outcome is persisted. Packets need `SO_BROADCAST`, no capabilities. `/v1/wol/neighbors` reads `/proc/net/arp` on request (admin only, rare), the one deliberate exception to "never on the request path".

**Event log** (`events/`). An append-only `events` table in `state.db`, plus the `problems` open now. Detectors only *report*: `Event` (happened once), `Open(problem)` (true now, repeat freely), `Resolve(key)` (seen clear). One task owns the open set and writes rows, so repeats are free and a problem opened before a restart stays open until its detector sees the good condition; that is also where hysteresis comes from (disks warn at 85%, clear below 80%). Detectors are pure and tested with plain timestamps: `containers.rs` (Docker event stream forwarded by `spawn_event_watcher`, plus a 15 s tick over the typed container list), `backups.rs`, `disks.rs`, `lifecycle.rs` (version, boot time and a clean-stop marker written after SIGTERM, which is how a power cut shows up). **A `die` with no `kill` in the previous 30 s is one nobody asked for**: every intentional stop (Compose recreating on deploy, `docker stop`, Helion actions) sends `kill` first, which is what keeps a deploy from reading as a wave of crashes. The event watcher subscribes 30 s in the past and marks those events `replayed`; the tracker only takes `kill`/`oom` from them (`ContainerTracker::replay`), so a `kill` sent while the agent itself was being recreated still counts, and nothing is reported twice. Admin actions are recorded in the write handlers via `api::record_action`, with who. `GET /v1/events` reads SQLite (`after=` polls forward, `before=` pages back); the app polls rather than streams, because each node already holds three of the browser's ~6 HTTP/1.1 connections. Several snapshots (`ContainerSnapshot`, `HostSnapshot`, `BackupSnapshot`) keep typed data alongside their JSON for these detectors.

**Notifications** (`notify/`). Every stored event is broadcast; the notifier offers it to each channel (ntfy, webhook), which takes it when `rules::wants` (enabled, category, `min_severity`, `recoveries`) and the `FloodGuard` agree: a problem opens at most once per 30 min per channel (a held-back opening is delivered late if the problem is still open, so a flap that sticks isn't left looking resolved), and the same one-off event (one container crashing) at most once. Delivery is a task per message, three tries, no 3xx or 4xx retry, then dropped: the log has it. The client doesn't follow redirects: a redirected POST becomes a GET, which once made an `http://` ntfy address that redirects to `https://` read as delivered. Channels are edited in the app and stored in `state.db`; **the channel secret is write-only** (`has_secret` in the API, hidden from `Debug`), the agent's one stored credential. The notifier must subscribe before anything reports, or it misses the lifecycle events at startup.

**Icons** (`icons/`). Every container gets an icon from homarr-labs/dashboard-icons without anyone choosing it (`[icons]`, on by default). `ContainerInfo.icon` is a name the agent already holds: the sampler calls `IconsHandle::resolve` (a map lookup; never I/O), which matches in `matcher.rs` (pure, tested on real images): the `helion.icon` label (`none` opts out), then `helion.service`, the image's repository name, the compose service and the container name, each exact and then without an ending like `-server`/`-ce`. The service key goes before the image so every container of a service (an app and its database) agrees; no fuzzy matching, and the source's free-text aliases are ignored. `helion`, `cosmos`, `atlas`, `solstice`, `solstice-sync` and `sunstead` never match: the app draws those from `@sunstead/ui`'s `ProductIcon`. A match whose file isn't on disk is queued; a task fetches it (and the variants its `colors` names, from `<source>/svg/<name>.svg`, plain SVGs under 512 KB), then pokes the container sampler so the name appears on the next list. The index (`<source>/metadata.json`, trimmed to SVGs) is kept in `<state dir>/icons/index.json` and refetched every `refresh_days`; with the source down, what is cached keeps serving. `GET /v1/icons/:name?theme=dark|light` serves the file, choosing among the icon and its variants by the *ink* (`ink.rs` reads the fill colours), since the source's `-light`/`-dark` names are not consistent (`github-light` is white, `portainer-dark` is near-white). Names are validated as file names (`index::valid_name`). A container with no match is logged once at info.

**Uptime** (`uptime/`). One task owns the checks, like Wake-on-LAN's: every `helion.service` with a url label gets an HTTP check (`svc:<service>`, `https://` added as `serviceHref` does), and custom HTTP or TCP checks live in `state.db`, as do per-service settings (off, path, any status counts). A service's check only runs while one of its containers is running, and failures in the first 2 minutes after a container starts don't count, so a deliberate stop or a deploy never pages. A service whose containers vanish keeps its check, paused, for a minute (`carry_vanished`), so a deploy that removes and recreates them doesn't resolve its problem or reset its heartbeat. Probes run in their own tasks and report back, so a slow target doesn't hold up a reload; creating or editing a check waits for its first result. HTTP doesn't follow redirects (a forward-auth 302 is up) and is up below 400. `[uptime] local_domains` resolves those names to 127.0.0.1 through a custom reqwest resolver: on Jupiter the home domain's names point at the Tailscale IP, which the host can't reach, so checks go through Caddy on the host. `detector.rs` is pure: down after 3 failures in a row, up after 2 passes, and certificates (read from `tls_info` by the small DER walker in `cert.rs`, keyed by serial so a wildcard is one problem) warn in their last quarter and are an error under 3 days. Results are tallied per hour into `uptime_hourly` every 5 minutes (additive, kept 90 days) for the 24h/30d/90d figures; the newest 90 per check live in memory for the heartbeat bar, and outages themselves are in the event log.

**Peers** (`peers/`). Agents watch each other, since nothing on a node can
report that node down. `[[peers]]` lists the others; the side with a `url`
POSTs a `PeerHello` to the other's `/v1/peer/heartbeat` every 15 s and gets
one back, so one exchange tells both sides and only one has to reach the
other (Pluto dials Jupiter's agent through Caddy's public name for it: Jupiter's
Tailscale is userspace and can't reach Pluto, and Pluto may only reach
`jupiter:443`). The route is public and checks `HELION_AGENT_PEER_TOKEN`
itself (constant time, env only, the agent refuses to start with peers and no
token), not a sign-in: an Authentik restart must not read as a node down. One
task owns the trackers; `detector.rs` is pure: late after 30 s, down after
60 s (5 min after a hello with `stopping`, which the dialler sends on
SIGTERM), back after two heartbeats in a row. Down opens
`peer:<name>:unreachable` (`EventCategory::Peer`), with the dial error and
whether the tailnet sees the machine online. A problem can `depends_on` a
peer (an uptime check with `via_peer`); the notifier holds its opening
(`PeerHolds`) while that peer is down or back for under `SETTLE_SECS`, then
sends it if still open, or nothing if it cleared. `/config.json` lists each
peer's `ui_url` after this agent, and `/v1/info` carries `node_description`.

**Updates** (`updates/`). The agent never edits compose or pushes: git stays the record of what runs. Every `check_interval_hours` it lists each image repository's tags anonymously (`registry.rs`: one flow for every registry, the 401 challenge then a pull token from its realm, `Link` pagination up to 50 pages since Immich has ~30k tags, and **never a manifest request**, which Docker Hub counts against the pull limit deploys need). `rules.rs` groups services into units (one image repository, or a `helion.update.group`) and applies the `helion.update.*` labels (off, group, hold, major-step, major-digits, backup; the stricter wins); `tags.rs` only offers tags of the same shape (`16.15-alpine` → `16.x-alpine`, never an rc). First sighting of each tag is stored (`update_seen`) for `min_age_days`. Applying one dispatches the infrastructure repo's `update.yml` (`github.rs`, a fine-grained token from `HELION_AGENT_GITHUB_TOKEN` only, hidden from `Debug`), which re-checks everything itself, backs up where labelled, commits and deploys. A run is stored at each step (queued → dispatched → running → watching → done / failed / broken) so it resumes after the deploy restarts the agent. One workflow runs at a time, but the next queued one starts as soon as the last has deployed, while that one is still being watched; while a workflow runs, the agent reads its jobs for the step under way (`UpdateRun.step`, not stored), so the page only says it's backing up when it is. During the watch (`decide.rs`, pure) an uptime check going down or a container restarting marks it broken: a problem, and automatic updates for the unit pause until a policy is chosen again. Nothing rolls back on its own; a rollback is the same flow to the previous tag. Automatic updates are queued when the backup status shows a successful *timer* run (the timer fired just before it), so the nightly is the pre-update backup. `helion-agent --check-config <file>` lets the workflow try a new agent against the live config first.

**Backups** (`backups.rs`). The agent **does not run restic**. It reads a status JSON that `scripts/helion-backup-status.sh` writes on the host, so the repo password never enters a network-facing process and nothing can contend for the repository lock. `BackupsStatus.stale` and `timer_last_fired` exist to catch the case a snapshot list can't: a timer that fired but whose job died before writing status. **Requests** (0.7): "Back up now" and "Run test now" (`POST /v1/backups/run`, `backup_actions`) write `{id, kind, requested_by}` as `<id>.tmp` then rename it to `<id>.json` in `[backups] requests_dir`, the host's inbox mounted writable; a host service (Jupiter's `request-dispatch.sh`, started by a systemd `.path` unit) starts the matching unit and writes a result per request into `request_results_dir`. The sampler lists queued files and results as `requests`, and polls every 2 s while one is queued or running. The `backup_state` and `backup_dump` kinds are for updates and refused on this route (`backup_dump`: only the databases the updated services use, which is what an update takes now). The latest restore test comes from `restore_test_file` (path-free messages); a failed one opens `backup:restore_test`, and none for 15 days a warning. The restore guide in the app only generates commands for the host's `scripts/restore.sh` (`lib/restore.ts`), using the `databases` the status file lists; nothing restores from Helion.

## Deployment

Production is the Jupiter repo (`Sunstead/Jupiter`, usually checked out
beside this one), where a push to `main` deploys. It pins the agent to a
semver tag, bumped by hand in a PR (`compose/helion.yml`) once a `v*` tag has
published; Renovate is gone.
An agent refuses config it doesn't understand, so a release that changes
`agent.toml` needs its image published before the Jupiter PR that uses it
merges.

`helion-agent/compose.example.yaml` and `agent.example.toml`. Two things are easy to get wrong:

- **`network_mode: host` is required, not optional.** `/proc/net/dev` renders from the reading process's network namespace and sysinfo has no `HOST_PROC` escape hatch, so without it the agent reports the container's veth. Consequence: `ports:` does not apply, and `[host] net_exclude` becomes load-bearing.
- **`:ro` on `docker.sock` is not a security control** — it applies to the inode, not the protocol. Anyone who can `connect()` has the full Docker API, which is root-equivalent. The real controls are `allow_actions = false` and network isolation.

Disks (`sample/filters.rs`): in a container, the `[[host.disks]]` entry labelled `/` (or `[host] root`) says where the host root is mounted. The `/:/host/rootfs:ro,rslave` bind is recursive, so every other drive is discovered under it and reported by its host path; listing only the root used to hide them (a 2TB `/srv` never showed). Virtual filesystems, container storage and boot partitions are skipped, and mounts sharing a device are counted once (`pool_key`: the APFS container on macOS, the device path on Linux).

**Web UI from the agent.** The image sets `HELION_AGENT_WEB_DIR`, and
`api/web.rs` serves `app/dist` as a public fallback after the API routes:
SPA fallback to `index.html`, 404 for unknown `/v1/*` and missing `/assets/*`,
immutable caching for hashed assets. `/config.json` returns `[{"url":"/"}]`,
which the frontend resolves to its own origin; a seeded node that needs a sign-in
opens Add node prefilled. Same origin, so no CORS entry is needed.

Workspace `[profile.release-agent]` adds `panic = "abort"`; it is deliberately *not* in `[profile.release]`, which would also apply to `app/src-tauri` where Tauri uses `catch_unwind`.

## Frontend architecture

The app talks directly to agents from the webview; no Tauri IPC for data.

**One connection per node, owned by the store.** `api/connection.ts`
`NodeConnection` owns an agent's host stream, container stream, volume polling,
capability negotiation, backoff reconnect and status (`connecting | online |
offline | unauthorized`). Connections live in a `Map` outside React state
(`stores/nodes.ts`), so navigation never tears a stream down. EventSource gives
up permanently on a non-2xx and never exposes the status, so `connect()` probes
with `getHost()` first and reconnects explicitly.

**The node store must not churn.** `NodeConfig` is `{ id, url, agentName,
alias }`; display name is `alias ?? agentName ?? url` (`nodeDisplayName`,
`useNodeName`). `attach()` compares against *current* state and writes only on
a real change. A per-sample write re-renders the sidebar and every page and
rebuilds the constellation (the old "flash" bug; there's a regression test).

**Keep 1 Hz data out of React.** `<LiveValue>` and `<Sparkline>` subscribe to
the stream and write through refs inside a shared animation frame
(`lib/frame-scheduler.ts`), fed by ring buffers (`lib/ring-buffer.ts`,
`stores/metrics-history.ts`). `useHostInfo` re-renders every second; use it
only where the whole view changes. Recharts is only on `/monitoring`, which is
lazy-loaded.

**Adding an endpoint:** type in `helion-common` → route + module in
`helion-agent` → method on `AgentClient` → subscription in `NodeConnection` or
hook in `queries.ts` → store/page. Gate the UI on the `capabilities` flag so
older agents degrade instead of erroring.

**Service model.** Services come from the `helion.service` Docker label,
grouped per node in `lib/services.ts`, then across nodes by that key
(`groupServices`, the containers store's `groups`): a service on several nodes
(Helion itself) is listed once, with a row per node, and counted once. The
constellation keeps a moon per planet, since that's where each one runs.
Updates group the same way (`groupUnits`). `helion.service: system` marks
infrastructure (filtered by `userFacingServices()`). `helion.service.url` holds
bare hostnames; always route them through `serviceHref()`.

**Node scope.** A page that looks at one node at a time picks it with
`useNodeScope(page, { capability, allowAll })`: each page keeps its own choice
(`helion-scope-<page>`), offers only nodes with its feature (Backups never
offers Pluto), and `ALL_NODES` is the "All nodes" entry. Monitoring then draws
a line per node per metric (`mergeSeries` snaps them to a shared step), and
Logs merges every node's all-containers socket, lines tagged with `node`.
Events and Uptime use it as a filter, defaulting to all. Dialogs that act from
a node (uptime checks, Wake-on-LAN targets) make you pick it when there's more
than one. Notification channels are one list across nodes; a new one can be
sent to several, each node storing its own copy.

**Nodes bootstrap.** `router.tsx` root `beforeLoad` seeds from `config.ts`
(nothing under Tauri; `/config.json` in a browser). `onRehydrateStorage` runs
*during* `create()`, so it seeds through the draft and defers `attach()` to a
microtask.

## UI conventions

**Shell.** `layouts/AppLayout.tsx` mounts the title bar, sidebar, command
palette, add-node dialog and `CommandHost` (shortcuts + native menu events).
The sidebar's foot is `AccountMenu`: who you are, role, a gear for Settings
(which isn't in the nav list) and a menu with settings, a theme picker and
sign in or out. `UserAvatar` (also in Settings) shows the access token's `picture` claim,
only if it is `https://` (`setSession`), else initials on `--muted` (neutral in every theme; the sidebar accent isn't, Concrete's is
yellow) with a `foreground/10` ring, never the primary colour. Text on a
coloured fill always uses that fill's own foreground: Concrete's
`sidebar-accent-foreground` is near black, so it vanishes on the plain sidebar.
Base UI swaps the image in only once it has loaded, so a 404 or an offline
provider never shows a broken image. Jupiter sets no picture, so it shows
initials. Title bar controls are all ghost `Button`s so they match.
Nav items (`app-sidebar.tsx`) are default-size `h-8` buttons, rounded all
round and inset from the edges; the current page is a fill
(`sidebar-accent`), never an edge bar, and the rest are `muted-foreground`
so the fill reads in light themes too. Collapsed, each icon is centred in
the 4rem rail, under the title bar's toggle and over the avatar. The sidebar has no border of its own
(`group-data-[side=left]:border-r-0`, the package's own variant, so `cn`
replaces its `border-r`; a plain `border-r-0` is less specific and loses,
which drew a second line in 0.12.0): it runs into the title bar, and
`<main>`'s `border-t border-l` with its rounded top-left corner draw the
only edge.
Routes are lazy (`lazyRouteComponent`) and pick a layout through
`staticData.layout`: `scroll` (default), `fill` (logs) or `bleed` (constellation:
no padding, no header, edge to edge in `<main>`). The body never
scrolls; `<main>` does. Content is a `@container`, so layouts use container
queries (`@xl`, `@3xl`, `@5xl`), not viewport breakpoints.

**Pages are built from shared primitives**, not bespoke markup:
`PageHeader` (always rendered, even when empty, except on `bleed` pages; the row is `h-8` and every
header control is `h-8` so titles align), `SegmentedControl`, `SearchInput`
(`/` focuses it), `NodeSelect`, `StatCard`/`StatRow`, `Section`, `DataTable`,
`EmptyState` (`page | card | inline`) with an optional `SetupHint` (`?`
popover holding the config snippet), and sonner toasts. Use the shared
primitives before writing new ones: `@sunstead/ui/components/<name>` (shadcn
`base-vega` on Base UI, the same set Atlas and Solstice use), pinned to a tag
in `app/package.json`, and changed in the sunstead-ui repo, not here. Base UI
composes with `render={<Button />}` rather than `asChild`; menu items take
`onClick`; a `Select` shows its items' labels only when given `items`; a menu
label sits inside a `DropdownMenuGroup`. Links styled as buttons put
`buttonVariants(...)` on the `<Link>`. The sidebar is the package's
`resizable-sidebar`; `AppLayout` keeps its open state and width in
localStorage (`helion-sidebar-open`, `helion-sidebar-width`). An e2e spec
checks header height and title position on every page.

**Copy.** Short, sentence case, no em dashes or curly quotes. An ESLint rule
rejects them in JSX text and string literals under `pages/` and
`components/`. Missing values render as `NO_VALUE` (`n/a`).

**Themes.** The shared Sunstead set from `@sunstead/ui` (22, the same in
Atlas and Solstice), each with one scheme (dark or light) and one style
(`rounded` or `tech`), no custom ones. Themes, their CSS, `ThemeProvider` and
`useTheme` all live in that package; a new theme or token goes there (its
`themes.test.ts` checks every theme defines the same tokens, WCAG AA, metric
colours apart, and so on). Helion keeps the first-paint script in
`index.html` (ids, schemes and styles, and the aliases), which sets
`data-theme`, `data-style`, the scheme's `dark`/`light` class (Tailwind's
`dark:`, sonner) and `color-scheme` before anything renders;
`lib/themes-sync.test.ts` keeps it in step. **Tech** themes (Hologram,
Terminal, Red Alert, Blueprint) restyle the app from `[data-style='tech']`,
CSS only: square, bracketed panels, mono headings and numbers. They are
grouped under Dark or Light like the rest, and Follow system can pair them.
Storage: `helion-theme` (an id, or `system` to follow the OS) plus
`helion-theme-light`/`helion-theme-dark` for Follow system. Old values map
through `lib/theme-aliases.ts`: the pre-0.10 `dark`/`light`, and
`cosmos-dark`/`cosmos-light` from before the shared themes, all read as
Sunstead Dark or Light. `useThemeStyle` reads the style off `<html>` (works
without the provider). Themes are picked in Settings (Dark and Light cards,
previews scoped with `data-theme` and `data-style`) or the account menu;
there is no toggle or shortcut.

**Tokens.** Everything is themed through CSS variables, defined by every
theme: semantic colours, metric colours (`--cpu`, `--ram`, `--network`,
`--disk`), `--ansi-*`, `--mark-*`, `--glass-*`, canvas tokens (`--space`,
`--star`, `--orbit`, `--planet-*`) and the constellation tokens
(`--holo-space`, always dark, a touch deeper than the page in rounded themes;
`--holo-primary`, `--holo-secondary`, `--holo-text`, `--holo-dim`,
`--holo-glow` 0..1; from the theme's own accent in rounded themes, the full
hologram in tech ones). Layout
tokens (`--titlebar-height`, `--titlebar-inset`)
stay in `App.css` (`--text-2xs` and `--radius` come from the package). Canvas and WebGL code never uses literal colours; it reads them
through `lib/theme-tokens.ts`, which caches per theme id and fires
`onThemeChange` when `data-theme`, `data-style` or the scheme class changes; three.js gets
them via `lib/constellation/colors.ts` (three can't parse `oklch()`).
`.glass` is applied deliberately, `.label-hud` is the small-caps label, and
`.chrome` disables text selection on UI chrome (content stays selectable).
Container log colours go through `lib/ansi.ts` (`--ansi-0..15` tokens); other
escapes and control characters are stripped, and search/download use
`stripAnsi`. Never colour a log line by stream: lots of healthy software
writes everything to stderr. `logLevel` (`lib/log-line.ts`) reads the level
the line states; times render in the viewer's zone via `formatLogTime`
(Docker's nanosecond stamps are cut to ms first, since WebKit's `Date.parse`
has rejected longer fractions). Each line is prepared once on arrival (`lib/log-view.ts`
`prepareLine`: ID, level, times, search text, numeric sort key), merged by
time into the buffer rather than re-sorted, and keyed by its ID; the list is
virtualized (`@tanstack/react-virtual`), so only the rows on screen render. A
JSON line (`lib/structured-log.ts`) shows its message, or `GET /path 200 9ms`
for an access log, then the other fields faint, and opens to every field;
its level comes from its own `level` field.

**Loading.** Until a node sends its first containers or volumes, a page shows
skeletons (`components/skeletons.tsx`, `DataTable loading`), never an empty
state that isn't true. `useAwaiting(byNode)` says when: some connecting or
online node has no entry yet, capped at `AWAIT_MS` so a node that never
answers falls back to the real empty state.

**Planets.** `lib/planet.ts` gives each node name a deterministic style
(presets for jupiter, saturn, mars, etc.; seeded otherwise).
`lib/planet-render.ts` draws it as a 2D sprite for `NodePlanet`; the
constellation reads the same style for its wireframes (rocky bodies are
geodesic, gas and ice giants have graticules, ringed ones keep their rings).

**Constellation** (`lib/constellation/`, three.js). The cluster in 3D: nodes
are planets sized by memory with a CPU and memory gauge, services are moons,
tailnet devices sit on a tilted outer ring (shape by OS, waking pulses). It
follows the theme's style (`SceneOptions.style`, `setStyle`; every body has
both looks built and the style picks by visibility). **Rounded**: planets are
their avatar sprites (`planetSprite` on a billboard), moons solid dots (warning
or error colour when unhealthy), devices solid shapes; orbits, the gauge,
legend and hint stay; no floor grid, drop lines, glow or wireframes; panels are
context menus (`.sky-panel` points the `--holo-*` variables at popover tokens).
**Tech**: wireframe and fresnel planets, moons with a halo in trouble, spinning
wireframe probes, the floor grid, bracketed `.holo-panel`s and still
`.holo-scanlines`. Labels (`.sky-label`) sit on whole pixels, anchored from
their measured size, never `translate(-50%)` (that blurred the CPU line), and
their layer is `isolate` so their z-indices stay under the HUD. It's the Overview card and the `/constellation`
page, under Overview in the sidebar (`bleed`: a window into space filling the
content area, with no header or border, and an `sr-only` h1; also `G then X`,
the palette, or the card's expand button, which carries the focused node as
`?node=`). Clicking a node or a device focuses it (the camera follows it),
and a second click opens it; a moon opens at once. Nothing flickers, sweeps or scans: bodies hold steady.
Pieces:
`model.ts` is pure and tested (reconcile a `Snapshot` into a `Layout` keeping
phases, moon shells, focus order, the focus-then-open `interact` reducer,
screen-space `pick`); `source.ts` builds snapshots from the stores and only
notifies on a real change, and fans host samples out per node; `scene.ts`
owns the renderer, camera, `OrbitControls`, flights and the loop;
`materials.ts` holds the shaders; `hud.ts` puts real `<button>` labels over
the bodies, which is the keyboard and screen reader path (Tab walks nodes and
the focused node's moons, arrows walk everything, Enter focuses then opens,
Escape backs out, + and - zoom). `components/constellation.tsx` checks WebGL
(an inline empty state without it) and lazy-loads `constellation-view.tsx`,
so three.js is its own chunk. Rules it keeps: data reaches meshes from
subscriptions, never React renders; the scene makes its own canvas (a
context lost on purpose can't be re-made, which StrictMode would do);
resizing draws inside the ResizeObserver callback so the canvas never shows
blank; frames go at the display's refresh rate while anything moves (nothing
off-screen or hidden, only on change under reduced motion, which also stops
twinkle, auto-rotate and flights), at the screen's pixel density (up to 3x,
re-read by a resolution media query when the window changes screens), and
labels snap to device pixels, not CSS ones; `dispose()` releases the
context with `forceContextLoss()`.

**Platforms.** Tauri builds the window in Rust (`src-tauri/src/window.rs`),
not `tauri.conf.json`. An init script sets `window.__HELION_PLATFORM__`;
`main.tsx` copies it to `html[data-platform]` (`macos | windows | linux |
web`) before render. macOS: overlay title bar, traffic lights positioned by
`TRAFFIC_LIGHTS`, a native menu (must keep the Edit items or ⌘C/⌘V break in
inputs) that emits `menu` events, and `CommandHost` skips its own keydown
handling there to avoid double-firing. Windows/Linux: `decorations(false)` with
custom controls in `TitleBar`. The Rust side emits `window-state` (fullscreen,
maximized). Shortcuts live in `lib/shortcuts.ts` and feed the palette, key
hints and menu. Pages are bare-key sequences ("G then a letter", X for the
constellation, tracked by
`SequenceTracker`), handled by `CommandHost` on every platform and never in a
text field, dialog or menu; the macOS Go menu has no accelerators because a
menu can't show a sequence. Keep non-component exports out of component files
(hooks in `hooks/`, data and helpers in `lib/`), or fast refresh lint warns.

**App updates** (not the Updates page, which is about service images). The
desktop app updates itself with `tauri-plugin-updater` from `latest.json` on
the newest *published, full* GitHub release (`plugins.updater` in
`tauri.conf.json`); tag builds sign the bundles (`tauri.updater.conf.json`,
the `TAURI_SIGNING_*` secrets, see RELEASING.md). `lib/app-update.ts` wraps
the plugins (imported lazily, so the web bundle never loads them) and
`stores/app-update.ts` owns the state: a check 10 s after launch and every
6 h while `autoCheck` is on (persisted), a quiet failure for background
checks and the dialog for manual ones, Later per version. The title bar's
`AppUpdateIndicator`, `AppUpdateDialog`, **Check for App Updates…** in the
macOS app menu, the palette and Settings > About drive it. Desktop only (a
phone updates through its store): in a browser the same indicator appears
when the agent serving the page (`servedUpdate`: the node on this origin)
reports a newer version than `__APP_VERSION__`, and offers a reload.

**Service icons** (`lib/service-icons.tsx`, `lib/agent-icon.ts`). `ServiceIcon` takes the service key and, for a service, the agent's icon (`ServiceInfo.icon`, the container icons' most common; `ServiceGroup.icon` adds the node that has it). Order: a Sunstead product (`isProduct` from `@sunstead/ui/products`, drawn by `ProductIcon`; `cosmos`, `cosmos-agent` and `helion-agent` are Helion), then the agent's file, then one bundled in `assets/icons/` (found by file name through `import.meta.glob`, and `icons/local/` overrides; they show before the agent's arrives and when it has none), then lucide `Server`. Nothing needs adding for a new service: the agent finds it (see **Icons** above). The agent's file needs the node's token, so `agent-icon.ts` fetches it through `AgentClient.getIcon` once per node, name and scheme, keeps the blob URL outside React state (like the connections) and shows it in an `<img>`, which also keeps each SVG's `<style>` from reaching the page. `ServiceIcon` works without a `ThemeProvider` (dark). The Sunstead products' icons come from `@sunstead/ui`; the mark here (`build-mark.mjs`) is the source of its Helion one.

**Helion mark.** `app/scripts/build-mark.mjs` (`npm run build:mark`) turns the
two Inkscape sources (`icon_simple.svg` light ink, `icon_simple_dark.svg` dark
ink) into `public/favicon.svg`, which carries both palettes and switches on
`prefers-color-scheme` (browser chrome follows the OS, not the app theme), and
`src/assets/icon-mark.svg`, whose stops read `--mark-0`/`--mark-1` so the
inline copy follows the app theme. The inline one deliberately has no `<style>`
element: an inline SVG's stylesheet applies to the whole document.

**macOS icon.** `app/src/assets/app.icon` (Icon Composer) is the source.
`app/scripts/build-mac-icon.sh` compiles it to `src-tauri/icons/Assets.car`
(Liquid Glass, shipped via `bundle.macOS.files` and `CFBundleIconName` in
`src-tauri/Info.plist`) and a padded full-size `icon.icns` fallback. Both are
committed; re-run the script after editing the icon (needs Xcode 26+ and Icon
Composer). The glass icon only shows in a bundled build, not `tauri dev`.

## Names before 0.14 (Cosmos)

Helion was Cosmos until 0.14, and deployments move over in steps, so the agent
and app accept the old names, each with a warning, until the 0.15 cleanup.
**Env** (`config.rs` `env`): `HELION_X` first, then the same name under
`COSMOS_`; `apply_env` takes a lookup so tests don't touch the process
environment. `COSMOS_AGENT_TOKEN`/`_TOKEN_FILE` exist only to reject a pre-0.3
setup, and there is no `HELION_AGENT_TOKEN`. **Labels** (`labels.rs`):
`helion.*` else `cosmos.*`, read at the sampler edge (containers, volumes,
Docker events), and `update_labels` renames `cosmos.update*` to `helion.update*`
so the rules only ever see the new names; each old label is warned about once
per process, not per container per tick. **Data** (`use_legacy_data_dir`): the
default databases live in `/var/lib/helion-agent`; where the new one is missing
and `/var/lib/cosmos-agent` has one, that is used. The image creates both
directories (so a stale mount is writable), which is why the test is for the
database file, not the directory. **App**: `lib/migrate-legacy-storage.ts`
copies `cosmos-*` local storage keys to `helion-*` once (marker, not every load,
or a sign-out would be undone), moving the refresh tokens; `index.html` falls
back to the old theme keys for first paint. **Not renamed on purpose**: the
`cosmos-dark`/`cosmos-light` theme aliases, the Authentik slug and client id
`cosmos` in Jupiter, ntfy topics, the `cosmos_agent_data` volume. The JSON field
names became `helion_service*` with no alias, so an app older than 0.14 shows no
service grouping against a 0.14 agent. The desktop app's identifier changed, so
it starts empty.

## Gotchas worth knowing

- **SSE events must stay unnamed.** The agent emits `Event::default().data(..)`
  with no `.event(..)`. A named event is only delivered to a matching
  `addEventListener`, never to `EventSource.onmessage` — which looks like a
  perfectly healthy open stream that no handler ever sees.
- `npm run build` fails on a fresh clone until `cargo test -p helion-common`
  has generated `app/src/generated/`.
- **Wrap a `DropdownMenuSubContent` in `DropdownMenuPortal`.** Left inside the
  menu, its `.glass` backdrop filter becomes the containing block for the
  submenu's fixed positioning and the menu's overflow clips it out of sight.
