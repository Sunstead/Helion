#!/usr/bin/env bash
#
# helion-backup-status.sh — publish restic status for helion-agent to read.
#
# Call this at the END of your backup job, after restic has finished and while
# the repository environment is still set:
#
#     /path/to/helion-backup-status.sh "$EXIT_CODE" "$DURATION_SECS"
#
# In jupiter-backup.service that looks like:
#
#     start=$(date +%s); set +e
#     restic backup ... ; rc=$?
#     set -e
#     /path/to/helion-backup-status.sh "$rc" "$(( $(date +%s) - start ))"
#
# Why this exists rather than letting the agent run restic itself:
#
#   * The repository password never enters a network-facing process.
#   * `restic stats` takes a repository lock. Polling it from a monitoring
#     agent could make a real backup fail — a monitor must never be able to
#     break the thing it monitors. Here the lock is already held by the job
#     that just finished, so the call is free.
#   * systemd is not reachable from inside a container without mounting its
#     private socket, which is equivalent to granting root on the host. The
#     timer's next run time therefore has to come from out here.
#
# Reads from the environment (all already set by a restic job):
#   RESTIC_REPOSITORY, RESTIC_PASSWORD / RESTIC_PASSWORD_FILE
#   HELION_STATUS_FILE   where to write       (default $BACKUP_PATH/restic-status.json)
#   HELION_TIMER_UNIT    timer to report on   (default jupiter-backup.timer)
#   KEEP_DAILY / KEEP_WEEKLY / KEEP_MONTHLY   (default 7 / 4 / 6)
#   HELION_PG_DUMP_OK / HELION_STATE_COPY_OK / HELION_SPACE_OK /
#   HELION_HEARTBEAT_OK                       "1" or "0", optional
#   HELION_STATE_COPY_MSG / HELION_SPACE_MSG  a message for that step, optional
#   HELION_OFFSITE_OK / HELION_OFFSITE_MSG    the copy that leaves the machine
#                                             (restic copy), once there is one;
#                                             unset, Helion says none is made
#   HELION_OFFSITE_LABEL  where it goes, for display ("pluto", "B2"); never a
#                         path or a URL with credentials
#   HELION_DATABASES     JSON: [{name, service, used_by}], for the restore guide
#
# Nothing secret is ever written to the output file.

set -uo pipefail

EXIT_CODE="${1:-0}"
DURATION_SECS="${2:-0}"

STATUS_FILE="${HELION_STATUS_FILE:-${BACKUP_PATH:-/srv/backups}/restic-status.json}"
TIMER_UNIT="${HELION_TIMER_UNIT:-jupiter-backup.timer}"
KEEP_DAILY="${KEEP_DAILY:-7}"
KEEP_WEEKLY="${KEEP_WEEKLY:-4}"
KEEP_MONTHLY="${KEEP_MONTHLY:-6}"

NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# GNU date (Linux) uses `-d @epoch`; BSD date (macOS) uses `-r epoch`. The
# target is Debian, but supporting both means this can be tested anywhere.
epoch_to_iso() {
  date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
    || date -u -r "$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
}

if ! command -v jq >/dev/null 2>&1; then
  echo "helion-backup-status: jq is required (apt install jq)" >&2
  exit 1
fi

# --- snapshots -------------------------------------------------------------
# --latest 20 keeps this bounded; the UI only ever shows a recent window.
SNAPSHOTS="$(restic snapshots --json --latest 20 2>/dev/null)" || SNAPSHOTS=""
[ -z "$SNAPSHOTS" ] && SNAPSHOTS="[]"

# Reshape to exactly the fields helion-agent expects, dropping everything else
# (restic includes the repository paths and program version; no reason to
# publish more than is needed). size_bytes is null here: a per-snapshot size
# needs `restic stats <id>` per snapshot, which is far too slow to be worth it.
SNAPSHOTS="$(printf '%s' "$SNAPSHOTS" | jq -c '
  [ .[] | {
      id:        (.id // ""),
      short_id:  (.short_id // (.id // "")[0:8]),
      time:      (.time // ""),
      hostname:  (.hostname // ""),
      tags:      (.tags // []),
      paths:     (.paths // []),
      size_bytes: null
    } ]' 2>/dev/null)" || SNAPSHOTS="[]"
[ -z "$SNAPSHOTS" ] && SNAPSHOTS="[]"

# --- repository size -------------------------------------------------------
# raw-data is the on-disk footprint after dedup and compression, which is the
# number worth showing. The lock is already held, so this costs nothing extra.
REPO_SIZE="$(restic stats --json --mode raw-data 2>/dev/null | jq -r '.total_size // empty' 2>/dev/null)"
[ -z "${REPO_SIZE:-}" ] && REPO_SIZE="null"

# --- next scheduled run ----------------------------------------------------
# `systemctl show -p NextElapseUSecRealtime` is portable across systemd
# versions in a way that `list-timers --output=json` is not. Value is
# microseconds since the epoch, or 0/absent when the timer is inactive.
NEXT_RUN="null"
if command -v systemctl >/dev/null 2>&1; then
  NEXT_USEC="$(systemctl show "$TIMER_UNIT" -p NextElapseUSecRealtime --value 2>/dev/null)"
  if [[ "${NEXT_USEC:-0}" =~ ^[0-9]+$ ]] && [ "$NEXT_USEC" -gt 0 ]; then
    ISO="$(epoch_to_iso "$(( NEXT_USEC / 1000000 ))")"
    [ -n "$ISO" ] && NEXT_RUN="\"$ISO\""
  fi
fi

# --- optional step results -------------------------------------------------
# jq rather than printf: a message can hold paths and quotes.
step_json() { # $1 = "1"/"0"/"" , $2 = label, $3 = message (optional)
  case "${1:-}" in
    1) jq -nc --arg at "$NOW" --arg m "${3:-}" '{ok: true, at: $at, message: (if $m == "" then null else $m end)}' ;;
    0) jq -nc --arg at "$NOW" --arg m "${3:-$2 failed}" '{ok: false, at: $at, message: $m}' ;;
    *) printf 'null' ;;
  esac
}
PG_DUMP="$(step_json "${HELION_PG_DUMP_OK:-}" "postgres dump")"
STATE_COPY="$(step_json "${HELION_STATE_COPY_OK:-}" "state copy" "${HELION_STATE_COPY_MSG:-}")"
SPACE_CHECK="$(step_json "${HELION_SPACE_OK:-}" "space check" "${HELION_SPACE_MSG:-}")"
HEARTBEAT="$(step_json "${HELION_HEARTBEAT_OK:-}" "heartbeat push")"
OFFSITE="$(step_json "${HELION_OFFSITE_OK:-}" "offsite copy" "${HELION_OFFSITE_MSG:-}")"
DATABASES="${HELION_DATABASES:-[]}"
printf '%s' "$DATABASES" | jq -e 'type == "array"' >/dev/null 2>&1 || DATABASES="[]"

# --- emit ------------------------------------------------------------------
# Written to a temp file in the same directory and renamed, so the agent — which
# polls this path — can never observe a half-written document. rename(2) within
# a filesystem is atomic.
TMP="$(mktemp "${STATUS_FILE}.XXXXXX")" || exit 1
trap 'rm -f "$TMP"' EXIT

jq -n \
  --arg     generated_at   "$NOW" \
  --arg     last_run       "$NOW" \
  --argjson next_run       "$NEXT_RUN" \
  --argjson last_exit_code "${EXIT_CODE:-0}" \
  --argjson duration_secs  "${DURATION_SECS:-0}" \
  --argjson repo_size      "$REPO_SIZE" \
  --argjson snapshots      "$SNAPSHOTS" \
  --argjson daily          "$KEEP_DAILY" \
  --argjson weekly         "$KEEP_WEEKLY" \
  --argjson monthly        "$KEEP_MONTHLY" \
  --argjson pg_dump        "$PG_DUMP" \
  --argjson state_copy     "$STATE_COPY" \
  --argjson space_check    "$SPACE_CHECK" \
  --argjson heartbeat      "$HEARTBEAT" \
  --argjson offsite        "$OFFSITE" \
  --arg     offsite_label  "${HELION_OFFSITE_LABEL:-}" \
  --argjson databases      "$DATABASES" \
  '{
     generated_at:   $generated_at,
     last_run:       $last_run,
     next_run:       $next_run,
     last_exit_code: $last_exit_code,
     duration_secs:  $duration_secs,
     repo_size_bytes: $repo_size,
     snapshots:      $snapshots,
     retention:      { daily: $daily, weekly: $weekly, monthly: $monthly },
     postgres_dump:  $pg_dump,
     state_copy:     $state_copy,
     space_check:    $space_check,
     heartbeat:      $heartbeat,
     offsite_copy:   $offsite,
     offsite_label:  (if $offsite_label == "" then null else $offsite_label end),
     databases:      $databases
   }' > "$TMP" || exit 1

chmod 0644 "$TMP"
mv -f "$TMP" "$STATUS_FILE"
trap - EXIT

echo "helion-backup-status: wrote $STATUS_FILE"
