import { BackupsStatus } from '@/generated/BackupsStatus';

export type BackupHealth = 'healthy' | 'running' | 'stale' | 'interrupted' | 'failed';

/**
 * A timer that fired and nothing reported since: a backup is either running
 * or died. Only after this long is it the second. Matches the agent's event
 * log (`events/backups.rs`), which opens a problem at the same point.
 */
export const INTERRUPTED_AFTER_MS = 6 * 3_600_000;

/**
 * The headline for a backup status. A backup that stopped running looks
 * healthy if you only list snapshots, so staleness and the timer cross-check
 * decide it.
 */
export function backupHealth(s: BackupsStatus, now = Date.now()): BackupHealth {
  const fired = s.timer_last_fired ? Date.parse(s.timer_last_fired) : NaN;
  const wrote = s.generated_at ? Date.parse(s.generated_at) : NaN;
  if (Number.isFinite(fired) && Number.isFinite(wrote) && fired > wrote) {
    return now - fired >= INTERRUPTED_AFTER_MS ? 'interrupted' : 'running';
  }
  if (s.last_exit_code !== null && s.last_exit_code !== 0) return 'failed';
  // A step can fail in a run that otherwise worked: a copy, not the snapshot.
  const steps = [s.postgres_dump, s.state_copy, s.offsite_copy, s.space_check, s.heartbeat];
  if (steps.some((step) => step && !step.ok)) return 'failed';
  if (s.stale) return 'stale';
  return 'healthy';
}
