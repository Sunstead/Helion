import { describe, expect, it } from 'vitest';
import { backupHealth, INTERRUPTED_AFTER_MS } from '@/lib/backups';
import { BackupsStatus } from '@/generated/BackupsStatus';

function status(partial: Partial<BackupsStatus> = {}): BackupsStatus {
  return {
    generated_at: '2026-01-01T03:00:00Z',
    stale: false,
    last_run: '2026-01-01T02:00:00Z',
    next_run: null,
    timer_last_fired: '2026-01-01T02:00:00Z',
    last_exit_code: 0,
    duration_secs: 60,
    repo_label: 'nas',
    repo_size_bytes: null,
    snapshot_count: 0,
    snapshots: [],
    retention: null,
    postgres_dump: null,
    heartbeat: null,
    ...partial,
  };
}

describe('backupHealth', () => {
  it('is healthy by default', () => {
    expect(backupHealth(status())).toBe('healthy');
  });

  it('reads a timer that fired after the last status as running, then interrupted', () => {
    const fired = Date.parse('2026-01-02T02:00:00Z');
    const s = status({ timer_last_fired: '2026-01-02T02:00:00Z' });
    expect(backupHealth(s, fired + 60_000)).toBe('running');
    expect(backupHealth(s, fired + INTERRUPTED_AFTER_MS)).toBe('interrupted');
  });

  it('flags a non-zero exit', () => {
    expect(backupHealth(status({ last_exit_code: 3 }))).toBe('failed');
  });

  it('flags a failed step even when the job exited 0', () => {
    const offsite = { ok: false, at: null, message: 'pluto unreachable' };
    expect(backupHealth(status({ offsite_copy: offsite }))).toBe('failed');
    expect(backupHealth(status({ offsite_copy: null }))).toBe('healthy');
  });

  it('flags staleness', () => {
    expect(backupHealth(status({ stale: true }))).toBe('stale');
  });

  it('ranks interrupted above failed and stale', () => {
    expect(
      backupHealth(
        status({ timer_last_fired: '2026-01-02T00:00:00Z', last_exit_code: 1, stale: true }),
        Date.parse('2026-01-03T00:00:00Z'),
      ),
    ).toBe('interrupted');
  });
});
