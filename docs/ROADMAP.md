# Sunstead Roadmap: Helion, Homepage, and the App Suite

Where my homelab and software suite are today, where they're headed, and the
principles guiding the work. It's a handoff brief for Claude Code sessions
working toward this plan.

**How this relates to other docs:** `CLAUDE.md` in each repo is the authority on
how the code works and its conventions. `docs/BACKUPS.md`, `docs/SSO.md` and
`.github/workflows/deploy.yml` in the Jupiter repo describe how backups, sign-in
and deployment work today. This roadmap covers *direction*. Where they disagree
about current behavior, trust those docs and flag the discrepancy here.

**Goal:** reduce reliance on third-party systems by running my own suite of
services, with Helion as the control plane, a homepage as the front door, and
independent apps that store data in open formats.

**Naming:** "Sunstead" is my company/brand name and the prefix for my services
(Sunstead Helion, Sunstead Solstice, etc.). It is not a separate system, server
or filesystem path.

---

## 1. Current state

*Last checked against both repos: 2026-09-30.*

### Jupiter (primary server)
- Debian 13. Disks:
  - 1TB NVMe. The root filesystem is one 411G partition (`nvme0n1p5`: 204G used,
    187G free) holding the OS, Docker (`/var/lib/docker`, so every named volume
    and every Postgres) and `/srv/backups`. What's on the rest of the NVMe is
    unconfirmed.
  - 2TB HDD (1.8T) at `/srv/storage` (`STORAGE_PATH`): about 190G used, laid
    out by data class (section 4): `data/` (Immich photos, OpenCloud files),
    `apps/` (Gitea, Immich's own dumps), `derived/` (Immich thumbnails and
    transcodes) and the state backup repository.
- Other nodes (Saturn, Mars, etc.) are planned. Servers are named after planets.
- Docker Compose stack: Caddy, Authentik (own Postgres), OpenCloud, Immich (own
  Postgres), Gitea (shared Postgres), ntfy, Tailscale, and the Helion agent/UI.
  Nextcloud, Uptime Kuma and Portainer were retired on 2026-09-29/30.
- Caddy serves `*.home.example.com` with wildcard TLS via Cloudflare DNS-01.
- Access is **Tailscale-only**. A Cloudflare Tunnel was tried and rolled back
  (SSL limits on second-level subdomains, 100MB upload cap).

### Deployment (working)
- **GitHub is the source of truth for git and deployment**, not the self-hosted
  Gitea. Self-hosting the thing that deploys everything else is too fragile.
- A **self-hosted GitHub Actions runner on Jupiter** runs `deploy.yml` on every
  push to `main` (or manually): reset the real checkout at `DEPLOY_PATH` to
  `origin/main`, `compose config -q`, `pull` with retries, `up -d
  --remove-orphans`, restart Caddy and helion-agent if their bind-mounted config
  is newer than the container, prune images.
- Secrets live in the server's `.env`, not in git.
- **Every image is pinned to an explicit tag** (no digests), including
  `ghcr.io/sunstead/helion-agent`. Watchtower and Renovate are gone: Helion
  proposes and applies updates through Jupiter's `update.yml` (3C), and the
  agent's own image is bumped by hand.

### Sign-in (working)
- Authentik at `auth.home.example.com` is the single login, with MFA
  (passkey or authenticator) required.
- Authentik is configured through **blueprints in git** (`authentik/blueprints/`).
  **Exception:** Immich's OIDC client has no blueprint; it was set up by hand.
- Groups: `homelab-users` (can use apps) and `homelab-admins` (admin inside apps
  that map it, including Helion actions).
- Helion, Gitea, Immich and OpenCloud use OIDC. No app uses Caddy forward auth
  any more; the snippet stays in the Caddyfile for the next one.
- Every service keeps a local-account fallback if Authentik is down (`SSO.md`).

### Backups (working; no offsite copy yet)
Details in Jupiter's `docs/BACKUPS.md`.
- Nightly at 03:00: a dump of each Postgres, then encrypted **restic** snapshots
  into two repositories on different disks. The primary (`/srv/backups/restic`,
  NVMe) holds everything: `STORAGE_PATH` minus `derived/`, the dumps, the small
  volumes and `.env`. The state repository (`/srv/storage/backups/restic-state`,
  HDD) holds the dumps, volumes and `.env`, so either disk can fail alone.
  Retention 7 daily, 4 weekly, 6 monthly. The primary is skipped below a
  free-space floor (`BACKUP_MIN_FREE_GB`) so it can't fill the root disk.
- A weekly restore test loads every dump into a throwaway Postgres and compares
  sample files; Helion shows it and can run it, a backup, or (before an update)
  a dump of one database, through a request inbox the agent never holds the
  password for.
- Each run writes `restic-status.json`, which the Helion agent reads for its
  Backups page (including staleness detection, which replaced the Uptime Kuma
  heartbeat).
- **The gap:** nothing leaves the machine. Losing the whole server loses
  everything; the next step is a `restic copy` to another drive, node or
  offsite target.

### Helion (see CLAUDE.md for detail)
- Cargo workspace: `helion-agent` (axum, port 7700), `helion-common` (shared types
  via ts-rs), `app/src-tauri` (Tauri 2 desktop; the Android target is unused).
- The agent provides: host metrics with SQLite history tiers, container and
  volume streaming, container actions behind `require_write` (admin principal +
  `allow_actions`), merged container logs over WebSocket, restic backup status,
  Tailscale status, Wake-on-LAN, a `state.db` for UI-edited state with
  append-only migrations, OIDC auth via Authentik, and capability negotiation so
  older agents degrade gracefully.
- Service discovery through the `helion.service` Docker labels (`system` marks
  infrastructure).
- **Each agent is independent; there is no central Helion server.** The app
  connects to every agent directly. Anything "server-side" runs per node.
- Jupiter runs agent 0.9.2 with `allow_actions = true`. The agent, web UI and
  desktop app share one version (`npm run release`, see `RELEASING.md`). The web UI is served by
  the agent at `helion.home.example.com`; on my iPhone I use that.

### Other Sunstead projects
- **Solstice:** Tauri/React markdown note-taking app with wikilinks. iOS sync is
  planned.
- **Starbook:** self-hosted personal CRM / contact tracker. It will be renamed
  and brought into the suite.

---

## 2. Target architecture

Three layers with clear responsibilities.

### Layer 1: Helion (control plane, admin only)
Helion runs things: nodes, containers, deploys, updates, backups, health,
routing, secrets, logs and notifications. Only admins can act; `homelab-users`
can view at most.

### Layer 2: Homepage (Sunstead Atlas, the front door for users)
Quick launching of apps and services, unified search across services, an AI
assistant that can query and act across services, and an index of service
content.

### Layer 3: Apps (custom and off-the-shelf)
Each app is its own service with its own data. Apps store data in **open formats
on disk** (markdown, `.ics`, `.vcf`, original photos, plain files), authenticate
through **Authentik**, and keep working when Helion or the homepage is down.

**Build vs. adopt:** custom where I can do meaningfully better or the scope is
manageable (Solstice, the contacts app, the homepage); established open-source
apps where it's too complex (drive, photos). Those still fit the suite: Helion
manages them, Authentik handles login, the homepage indexes them, and they keep
data readable on disk wherever possible.

### Shared platform services
Used by all layers but not features of any one app: identity (Authentik),
notifications (ntfy first), an event bus, the search index and the AI tool
gateway.

### Core principles
- **Blast radius:** breaking Helion must never take down user apps. Alerts
  shouldn't depend on Helion either: apps and scripts can post to the
  notification service directly.
- **Permissions:** homepage and app users never get admin functions.
- **Files are the truth:** apps are indexers or editors over open files wherever
  possible. Anything that must live in a database is exported regularly.
- **Git is the source of truth for configuration:** Compose files, the
  Caddyfile, agent config and Authentik blueprints. Secrets stay in `.env`.
  Settings edited from the Helion UI live in the agent's `state.db`.
- **Secrets stay out of network-facing processes.** The agent doesn't run restic
  or hold its password. The same goes for anything that can push to the deploy
  repo: that's root on the server (see 3C).
- **Runtime-agnostic Helion:** Docker/Compose for now. New agent modules consume
  the samplers' snapshots rather than calling Docker directly; the existing
  Docker code isn't refactored until a second runtime is real.

---

## 3. Priorities

Build order: **backup safety → notifications (with the event log) → health
checks → backup controls → updates → homepage.** Backup controls moved ahead of
updates (2026-09-26): an update's pre-update backup goes through the same
request mechanism, and a broken update is noticed by the health checks. One
release per stage: 0.6 health checks, 0.7 backup controls, 0.8 updates.

### 0. Backup safety (Jupiter only; no new hardware)
*In progress: Sunstead/Jupiter#15, with the Helion Backups page showing the
new steps.* Get the data into a shape where a new drive or an offsite target is
a config change, and close what can be closed with two disks:
- Stop backing up derived data (Immich `thumbs/`, `encoded-video/`).
- **Cross-disk copy of the state:** databases dumps, small volumes and `.env` are
  small; keep a second copy on the HDD, so either disk can fail alone.
- Guard the root disk: the backup refuses to run (and reports why) below a
  free-space floor, instead of filling `/`.
- One real restore test (files and a database dump into throwaway containers).
- Confirm `RESTIC_PASSWORD` and `.env` are in a password manager.
- Later, with hardware: point the originals repo at a separate drive, then
  offsite (`restic copy`).

### A. Helion: event log and notifications
*Done in 0.5 (Cosmos#13, #14; Jupiter#18, #19): the event log, ntfy on the
iPhone, desktop notifications and "node unreachable" alerts.*
- One append-only **event log** per agent in `state.db`: container died or
  restarted, health check failed or recovered, backup failed or stale, update
  applied, machine woken, and every admin action with who did it. The event
  timeline is a page over this log, and it doubles as the audit log 3E needs.
- **Notifications are routing rules** over events: which kinds, at what
  severity, go to which channels, with de-duplication and quiet recovery.
- Channels are pluggable. Always: the in-app inbox, and Tauri desktop
  notifications while the app runs. First external channel: **ntfy**, self-hosted
  on the tailnet (the iPhone app needs ntfy.sh as an APNs relay, which sees a
  message ID only). Kept open for: a generic webhook (Home Assistant, Gotify,
  chat apps), Web Push to the home-screen web app, email.
- Follows the agent's patterns: background tasks, never on the request path,
  capability flag, types in `helion-common`.
- **Watching the watcher:** nothing on Jupiter can report Jupiter being down.
  The desktop app notifies when a node stays offline, and since 0.11 agents
  watch each other: Pluto exchanges heartbeats with Jupiter, each reports the
  other unreachable, and ntfy runs on Pluto (public at `ntfy.example.com`) so
  an alert about Jupiter has somewhere to go.

### B. Helion: health checks (replaces Uptime Kuma)
*Shipped in 0.6 (Cosmos#15, Jupiter#23): an HTTP check per labelled service,
custom HTTP and TCP checks edited in the UI, certificate expiry, an Uptime
page. No ICMP: the agent runs with no capabilities, and TCP covers what ping
would. Kuma was retired on 2026-09-29 (Jupiter#31).*
- HTTP, TCP and ping probes per service with uptime history; targets default
  from the `helion.service` labels.
- Failures and recoveries become events, so they notify and show on the timeline.
- The backup heartbeat moves to the agent's own staleness check. Then retire Kuma
  (and skip its v2 upgrade).

### C. Helion: updates (replaces Renovate)
*Shipped in 0.8 and 0.9: the Updates page, rules as compose labels, and
`update.yml` in Jupiter as described below; Renovate is removed. Rollback of a service with a
database also points at the `pre-update` backup, since a downgrade may not read
a migrated database.*
No GitHub PRs to merge. Updates happen from a Helion screen or automatically.
- The agent checks registries for newer tags of the images it runs, on a slow
  schedule. The UI lists them with a release-notes link.
- **Per-service policy:** manual (default), patch automatically, or fully
  automatic. The policy is set in the Helion UI (`state.db`); the rules below
  live in compose labels (decided 2026-09-26).
- Update rules that Renovate encodes today (groups that move together, held
  majors, one Nextcloud major at a time) move into compose labels, so they live
  in git.
- **The agent never gets write access to the repo.** A token that can push to
  Jupiter's `main` is root on Jupiter. Proposed: approving an update makes the
  agent dispatch an `update.yml` workflow in the Jupiter repo with `(service,
  tag)`, using a token limited to triggering workflows on that repo. The
  workflow checks the request against the registry and the compose files, takes
  the pre-update backup, commits the bump and deploys. A commit made by a
  workflow doesn't trigger `deploy.yml`, so `deploy.yml` becomes a reusable
  workflow that `update.yml` calls.
- Rollback is the same flow with the previous tag.
- Renovate stays until this ships, then is removed.

### D. Helion: backup controls
*Shipped in 0.7 (Cosmos#16; Jupiter#22 for the host side): back up now, the
weekly restore test with its result in Helion, and a restore guide that
generates `scripts/restore.sh` commands.*
- **Trigger backups from the UI** without the agent holding the restic password:
  the agent writes a request file into a directory the host watches (a systemd
  `.path` unit), which starts `jupiter-backup.service`. The status file reports
  the result. The pre-update backup uses the same mechanism.
- **Guided restore:** show snapshots and walk through restoring files or a
  database per `BACKUPS.md`. A guide only: Helion generates the commands and
  you run them (decided 2026-09-26).
- **Automated restore drills:** periodically restore into a throwaway container
  and verify.

### E. Homepage (Sunstead Atlas)
*Live on Jupiter in its own repo (`Atlas`): 0.1.4, 2026-10-04. M0 to M6 are
done: Authentik sign-in, per-user connections, OpenCloud files from disk with
deep links and thumbnails, Immich, browser search, suggestions, the launcher
and keyboard, and Solstice notes (read from Solstice Sync's vaults on disk,
opened in the desktop app with `solstice://` links). Features are frozen
while it is integrated with the other services; next is shared project
spaces once any exist, and creating notes through Solstice Sync's API. Originals sit under `/srv/storage/data`, one folder per
app and user, so indexing reads them directly (section 4).*
- MVP sources: OpenCloud files (indexed from disk, the API for links,
  thumbnails and shared spaces) and Immich photos (its own smart search, at
  query time). Solstice notes since 0.1.4; calendar and contacts come later.
- App launcher built on `helion.service` label discovery.
- Unified search: full-text (Tantivy) plus embeddings later. Off-the-shelf apps
  indexed through their APIs or on-disk files.
- **Search and AI results respect permissions:** a user only sees data they own
  or that's shared with them.
- Sign-in through Authentik, reusing Helion's OIDC approach.
- AI through an **MCP gateway** exposing tools per service. Read-only first, then
  tiers: reads free; reversible writes need a quick confirmation; destructive or
  infrastructure actions always need explicit approval.
- Every AI tool call is written to the audit log (the event log from 3A).
- Content ingested from emails, documents and files is untrusted and never
  triggers actions on its own.
- The gateway is Tailscale-only and authenticated per client.

### F. Mobile
- My phone is an iPhone, so mobile means the web UI in iOS Safari (installable to
  the home screen) or a Tauri iOS target. The Android target isn't a priority.
- A mobile app for the homepage, likely also Tauri.

---

## 4. Filesystem and multi-user design

### Multi-user: design for it now, build for myself
- Authentik groups already exist. What's missing is **data ownership**.
- Every piece of data has an owner, encoded in its path and keyed by a **stable**
  identifier. Proposed: the Authentik username, treated as immutable (readable
  paths; Immich's storage label can take the same value).
- Helion stays admin-only for actions. Sharing starts with shared spaces (e.g.
  family photos), not fine-grained sharing.

### Data classes
| Class | Examples | Storage | Backup |
|---|---|---|---|
| Originals | photos, files, notes, calendars, contacts | HDD (`/srv/storage`) | restic + off-disk copy + offsite |
| State | databases, app configs | NVMe | Nightly dumps + restic, copied to the other disk |
| Derived | thumbnails, transcodes, search index | Anywhere with space | Not backed up (rebuildable) |

### Layout (built 2026-09-29/30; Jupiter's CLAUDE.md, Storage layout)
`/srv/storage` (the HDD) is split by data class. Small state stays in named
volumes on the NVMe; derived data stays on the HDD (the NVMe has limited free
space), excluded from backups.
```
/srv/storage/
  data/                      # originals: backed up, and what Atlas indexes
    photos/                  # Immich's upload location (library/<user>/ inside)
    files/users/<user>/      # OpenCloud personal spaces (PosixFS, watched)
    files/projects/<id>/     # OpenCloud shared spaces
    notes/<user>/<vault>/    # Solstice Sync's vaults (plain files)
    dav/<user>/              # calendars and contacts (when Radicale arrives)
    kin/<user>/              # contacts app (Starbook, renamed)
  apps/                      # app state on the HDD: gitea/, immich/backups/
  derived/                   # rebuildable: immich/thumbs, immich/encoded-video
  backups/                   # the state restic repository
  restore/                   # restore.sh scratch
```
OpenCloud's `users/` and `projects/` are its own default layout, kept rather
than templated to `files/<user>` and `shared/`, so upgrades don't fight it.
Folders are created when an app owns them, not ahead of time.
App-first, then per user, because that matches how containers mount data. Each
container mounts only its own folders. Off-the-shelf apps keep their own internal
layout inside their folder.

**Immich:** the storage template is already on
(`library/<user>/<year>/<day>/<file>`). Keep it. Its `thumbs/` and
`encoded-video/` are derived and can be mounted elsewhere; its `backups/` holds
Immich's own database dumps, which already sit on the HDD. Albums, people, faces
and favourites live only in its database, so the dumps and an API export matter.
Immich writes `.immich` marker files and refuses to start if they vanish, so
moving its folders is a planned migration, never an `mv`.

### Multiple drives and nodes
- Apps see paths by purpose (`/srv/storage/data/...`), never physical disks.
- New drives use **ZFS or btrfs** (checksums plus snapshots), with datasets per
  data class. mergerfs plus SnapRAID is the alternative for mismatched drives.
- Each dataset has a **home node**, and apps run next to their data. No databases
  over network mounts.
- Other nodes receive copies (restic copies or filesystem replication), not live
  access.
- Helion keeps a **storage registry** mapping logical names (e.g.
  `photos/<user>`) to node, path and data class, enabling service migration
  later.

---

## 5. Later

- **Calendars and contacts:** **Radicale** (plain `.ics`/`.vcf` under
  `data/dav/<user>`; Apple devices connect natively), when they're needed.
  Nextcloud's weren't in use, so its replacement by OpenCloud (done 2026-09-30)
  moved files only. CalDAV/CardDAV clients use app passwords; web UIs use SSO.
- **Immich readability:** XMP sidecars, maybe an external library so a plain
  folder is the source of truth. Export albums and faces via the API.
- **Starbook** joins the suite as a custom Sunstead app (renamed).
- **Solstice sync:** a dedicated CRDT sync service, enabling iOS Solstice
  (decided 2026-10-03; live at `solstice.home.example.com` since
  2026-10-04, desktop app first). Solstice is a monorepo: `apps/desktop` (the
  Tauri app), `apps/sync` (the server), and shared Rust crates. Yjs
  via `yrs`, one document per note over its raw markdown, so the `.md` files
  under `data/notes/<user>/<vault>` stay the truth and Atlas indexes them from
  disk. A vault manifest gives notes stable ids across renames. Overlapping
  offline edits are merged, flagged and offered for review. Design and the
  Automerge comparison: `Solstice/docs/sync.md`.
- **Service migration** between nodes and idle auto-sleep for nodes (building on
  Wake-on-LAN).

---

## 6. Decisions

| Date | Decision |
|---|---|
| 2026-09-24 | This roadmap lives at `docs/ROADMAP.md` in the Helion repo. |
| 2026-09-24 | Backup safety comes first, before any Helion feature work. |
| 2026-09-24 | Notifications: ntfy first, behind a pluggable channel design. |
| 2026-09-24 | Updates: a custom Helion flow plus auto-update policies, not Renovate PRs. |
| 2026-09-24 | Mobile means iOS / the web UI, not Android. |
| 2026-09-24 | Keep the Windows partition. The primary repo stays on `/`, guarded by a free-space floor (`BACKUP_MIN_FREE_GB`, 50). |
| 2026-09-24 | The state (dumps, volumes, `.env`) gets a second restic repo on the HDD, `/srv/storage/backups/restic-state`. |
| 2026-09-24 | Old snapshots holding Immich thumbnails age out; no history rewrite. |
| 2026-09-24 | Immich's `library/<user>/<year>/<day>/` storage template stays as it is. |
| 2026-09-26 | Stage A shipped in 0.5. The agent, web UI and desktop app share one version. |
| 2026-09-26 | Health checks: automatic per labelled service, extras and tweaks in the UI. HTTP and TCP only. |
| 2026-09-26 | Updates: rules (groups, holds, one major at a time) in compose labels; per-service policy in the UI. |
| 2026-09-26 | Pre-update backups only for services with a database. Auto-updates apply after a successful nightly backup, for versions first seen 3 or more days ago. |
| 2026-09-26 | A broken update alerts and offers rollback (with a database restore where one migrated); nothing reverts on its own. |
| 2026-09-26 | Restores from Helion are a guide only. |
| 2026-09-26 | Order: health checks (0.6), backup controls (0.7), updates (0.8), one release each. |
| 2026-09-29 | Homepage working name: Horizon. |
| 2026-09-29 | HDD layout by data class: `data/` (originals), `apps/` (HDD app state), `derived/` (rebuildable, not backed up). |
| 2026-09-29 | Files move to OpenCloud with PosixFS (plain files, watched), keeping its default `users/` and `projects/` folders; one Authentik client for all its apps. |
| 2026-09-29 | Uptime Kuma and Portainer retired; Helion covers both. |
| 2026-09-30 | Nextcloud retired, files only (no calendars or contacts were in use); a `nextcloud-final` snapshot is kept in both repositories. Radicale waits until calendars are needed. |
| 2026-10-02 | Homepage name: Sunstead Atlas (was Horizon). |
| 2026-10-02 | Homepage repo: its own (`Atlas`), with the shared UI package (`packages/sunstead-ui`, themes from Helion) starting inside it until a second app adopts it. |
| 2026-10-02 | Atlas signs in as a confidential OIDC client with a server-side session cookie, unlike Helion's public client and bearer tokens, so links and address-bar searches work through sign-in. Users paste per-service API keys, sealed at rest. |
| 2026-10-02 | Solstice Sync comes before Atlas indexes notes; Atlas creates notes through it rather than writing vault files. |
| 2026-10-03 | Atlas features are frozen while it is integrated with the other services (notes, shared UI). |
| 2026-10-03 | Solstice Sync is CRDT-based, and lives in a Solstice monorepo next to the desktop app. |
| 2026-10-03 | `sunstead-ui` moves to its own repo (`Sunstead/sunstead-ui`, 0.2.0) and standardises on Base UI; Helion and Atlas move off Radix, Solstice already uses it. |
| 2026-10-03 | Every repo lives in the `Sunstead` GitHub organization (Solstice and Jupiter moved from `InventorPWB`). Helion's update token is now owned by the organization. |
| 2026-10-03 | Solstice Sync uses Yjs (`yrs`), keeping history so overlapping offline edits can be flagged and reviewed. |
| 2026-10-04 | Solstice Sync is live on Jupiter (public OIDC client, 10-minute tokens; a sync connection closes when its token expires). Solstice is public under MIT. Atlas 0.1.4 searches notes from the vaults on disk. |
| 2026-10-10 | Multi-node: a service on several nodes is one entry with a row per node; pages that show one node keep their own choice, and Monitoring and Logs have "All nodes". |
| 2026-10-10 | ntfy moves to Pluto, public at `ntfy.example.com`, so the phone needs no tailnet and Jupiter going down can be reported. "Pluto down" goes from Jupiter to a secret ntfy.sh topic. |
| 2026-10-10 | Agents are peers (0.11): heartbeats with a shared token (not Authentik, which lives on Jupiter), dialled by Pluto. Checks that go through a peer are folded into its one alert. |
| 2026-11-03 | Blind spots, in three releases: 0.15 per-container history, missing services and pressure alerts; 0.16 acknowledge and silence, crash logs kept with the event; 0.17 temperatures, S.M.A.R.T. (a Jupiter host script) and offsite copy status. |
| 2026-11-03 | Expected services are learned from what ran (10 minutes running), not declared or read from compose; Forget retires one. |
| 2026-11-03 | Container history keeps 1m for 7 days and 5m for 30 days, keyed by container name. |
| 2026-11-03 | Silencing is acknowledge plus ad-hoc silences per node or service; no recurring windows. |

### Open
- **Starbook name:** Sunstead Kin (current favorite), Tether, Folk.
- **Infra repos per node:** one repo per server (like `Jupiter`) or one repo with
  a directory per node. Matters once Saturn exists, and for 3C.
- **Filesystem:** ZFS vs. btrfs when a second drive is added.
- **Backup copy target:** second local drive, another node, or offsite.
- **Image digests:** pin tag plus digest, or tags only (digests block silent
  rebuilds of the same tag, including security rebuilds, until Helion proposes
  them).
- **AI model:** local (Ollama), hosted, or hybrid.
- **Mobile:** Tauri iOS for Helion, and how the homepage app is built.

---

## 7. Instructions for Claude Code

### Working agreements
- **Read `CLAUDE.md` first** in each repo and follow its conventions. This
  roadmap doesn't override them.
- **Explore before changing.** Summarize what exists before proposing a
  reorganization.
- **Never run destructive commands against Jupiter, `/srv/storage` or the backup
  repository without explicit confirmation.** Data there is irreplaceable. Plan
  data migrations as written steps first.
- **Pushing to `main` in the Jupiter repo deploys to production.** Use branches
  and PRs.
- **Never commit secrets.** Keep them in `.env`. Authentik changes go through
  blueprints.
- Small, reviewable increments; propose a plan before large refactors.
- Keep this document updated as decisions are made, in the Decisions table.
