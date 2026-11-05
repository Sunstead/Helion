# helion-agent

The per-node half of Helion. Serves host metrics, Docker containers and
volumes, metrics history and read-only restic status to the Helion desktop app,
and (in the Docker image) the Helion web UI from the same address.

**Design in one line: sample on a schedule, never on the request path.**
Background samplers publish to `watch` channels and HTTP handlers hand out the
latest value, already serialized — so `GET /v1/host` does no collection, no
serde and takes no locks.

## Running it

```bash
HELION_AGENT_ALLOW_ANONYMOUS=true HELION_AGENT_BIND=127.0.0.1:7700 cargo run -p helion-agent
```

That runs it open, for development. Everything else has a working default
except sign-in: a real agent needs an OpenID Connect provider (`[auth.oidc]`,
or `HELION_AGENT_OIDC_ISSUER`) and refuses to start without one. See [`agent.example.toml`](agent.example.toml) for the full
configuration. Any string in it can name environment variables, `${NAME}`, so
domains and the like can live in your `.env` rather than in the file.

## Deploying to a Docker host

GitHub Actions publishes `ghcr.io/sunstead/helion-agent` (linux/amd64) on every
push to `main` (`:latest`, `:main`, `:sha-<short>`) and on `v*` tags
(`:1.2.3`, `:1.2`). To build it yourself, from the **repository root**:

```bash
docker build -f helion-agent/Dockerfile -t helion-agent .
# from an Apple Silicon Mac for an x86_64 server:
docker buildx build --platform linux/amd64 -f helion-agent/Dockerfile -t helion-agent .
```

Then use [`compose.example.yaml`](compose.example.yaml).

**Web UI.** The image bundles the web build and sets
`HELION_AGENT_WEB_DIR`, so `http://<host>:7700/` (or the Caddy address in
front of it) opens Helion. Static files are public; every API route still needs
the token, which the page asks for on first visit and keeps in that browser.
Unset `HELION_AGENT_WEB_DIR` (or `[web] dir`) for an API-only agent. The desktop
app connects to the same address.

Two things are easy to get wrong:

**`network_mode: host` is required, not a convenience.** `/proc/net/dev` is
rendered from the reading process's network namespace, and `sysinfo` has no
`HOST_PROC` escape hatch — without host networking the agent reports the
container's own veth instead of the machine's interfaces. Consequences:
`ports:` no longer applies (the agent binds `:7700` on the host directly), the
container is not on your bridge network, and `[host] net_exclude` becomes
load-bearing because the agent now sees every `docker0`, `br-*` and `veth*` the
host has.

**`:ro` on `docker.sock` is not a security control.** The flag applies to the
inode, not the protocol. Anyone who can `connect()` to that socket has the full
Docker API, which is root-equivalent on the host. The real controls are
`allow_actions = false` and network isolation.

**Keep a restart policy.** Compose bind-mounts the socket *file*, so when
dockerd restarts underneath a running agent (a Docker upgrade with
`live-restore` on) the container still holds the old, deleted socket and no
reconnect can reach the new one. After Docker calls have failed for a minute
with the socket path still present, the agent logs why, records an event and
exits non-zero, and `restart: unless-stopped` starts it with a fresh mount.
A missing socket means Docker is stopped, and the agent just waits for it.

Filesystems are listed explicitly in `[[host.disks]]`, because a container's
`/proc/mounts` is full of overlayfs and bind mounts. `path` is what the agent
sees, `label` is what the UI shows.

## Backup status

The agent **does not run restic**. Your backup job writes a small JSON status
file and the agent reads it. That choice is deliberate:

- The repository password never enters a network-facing process.
- `restic stats` takes a repository lock. Polling it from a monitoring agent
  could make a real backup fail — a monitor must never be able to break the
  thing it monitors.
- systemd is not reachable from inside a container without mounting its private
  socket, which is equivalent to granting root on the host.

### Wiring it into a restic job

[`scripts/helion-backup-status.sh`](scripts/helion-backup-status.sh) writes the
file. Call it at the end of your backup script, while the repository
environment is still set. For a script that uses `set -euo pipefail`, run it
from an `EXIT` trap so a *failed* run still reports — a backup that died is
exactly what you want to see.

For the Jupiter stack's `scripts/backup.sh`, the change is:

```bash
# ── after the RESTIC_REPOSITORY / RESTIC_PASSWORD exports ───────────────
HELION_START_TS=$(date +%s)
HELION_PG_DUMP_OK=0
HELION_HEARTBEAT_OK=0

helion_status() {
  local rc=$?
  HELION_STATUS_FILE="$BACKUP_PATH/restic-status.json" \
  HELION_TIMER_UNIT=jupiter-backup.timer \
  HELION_PG_DUMP_OK="$HELION_PG_DUMP_OK" \
  HELION_HEARTBEAT_OK="$HELION_HEARTBEAT_OK" \
  KEEP_DAILY=7 KEEP_WEEKLY=4 KEEP_MONTHLY=6 \
    "$PWD/scripts/helion-backup-status.sh" "$rc" "$(( $(date +%s) - HELION_START_TS ))" \
    || true    # never let status reporting fail the backup
}
trap helion_status EXIT

# ── after the three `dump` calls ───────────────────────────────────────
HELION_PG_DUMP_OK=1

# ── inside the heartbeat block, on success ─────────────────────────────
HELION_HEARTBEAT_OK=1
```

It needs `jq` (`sudo apt install jq`). Then set `[backups] enabled = true` in
`agent.toml`.

### Why `stale` and `timer_last_fired` matter

A backup system that quietly stopped running looks identical to a healthy one
if all you render is a list of snapshots — they just stop getting newer. The
agent computes `stale` from the status file's age against
`expected_interval_secs`, and separately reads the mtime of
`/var/lib/systemd/timers/stamp-<unit>`. If the timer stamp is **newer** than the
status file, the timer fired but the job died before writing — which the
snapshot list alone would never reveal.

Nothing secret is ever published: not the repository path, not
`RESTIC_PASSWORD`, not the Uptime Kuma push URL (which embeds a token).
`repo_label` is a display string from the config.

## API

| Method | Path | Auth | Write gate |
|---|---|---|---|
| GET | `/healthz` | – | – |
| GET | `/v1/info` | – | – |
| GET | `/v1/host` | ✓ | – |
| GET | `/v1/host/stream` | header or `?token=` | – |
| GET | `/v1/containers` | ✓ | – |
| GET | `/v1/containers/stream` | header or `?token=` | – |
| POST | `/v1/containers/:id/{start,stop,restart}` | ✓ | ✓ |
| DELETE | `/v1/containers/:id` | ✓ | ✓ |
| GET | `/v1/containers/:id/logs` | ✓ | – |
| GET | `/v1/containers/:id/logs/ws` | `?token=` | – |
| GET | `/v1/volumes` | ✓ | – |
| DELETE | `/v1/volumes/:name` | ✓ | ✓ |
| GET | `/v1/metrics` | ✓ | – |
| GET | `/v1/backups` | ✓ | – |

`/v1/info` is unauthenticated so the desktop app can tell "nothing is listening
here" from "an agent is here and wants a token" *before* it has one — but
`node_name` stays `null` until the caller proves itself, so an unauthenticated
scan can't harvest machine names.

Streaming routes accept `?token=` because browser `EventSource` and `WebSocket`
cannot set request headers. Query tokens leak into access logs, so the trace
layer records `uri.path()` only and the agent never issues redirects. A
short-lived ticket exchange would remove that caveat entirely and is the
obvious next step if the agent is ever exposed beyond a LAN.

Errors carry a typed envelope. The `not_enabled` (501) versus `unavailable`
(503) split is load-bearing: the UI hides a feature permanently for the former
and offers a retry for the latter. `/v1/containers` returns 503 when Docker is
down rather than an empty list, which the old agent could not distinguish from
"this host runs no containers".
