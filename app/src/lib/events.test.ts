import { describe, expect, it } from 'vitest';
import { alertsFor, categoryOf, dayLabel, mergeEvents, mergeProblems, wantedOnDevice } from './events';
import { event, problem } from '@/test/fixtures';

const resolved = event({ severity: 'warning', title: 'gitea is healthy again', problem: { key: 'k', state: 'resolved' } });

describe('wantedOnDevice', () => {
  it('matches the phone by default: problems and their recoveries', () => {
    expect(wantedOnDevice(event({ severity: 'error' }), 'problems')).toBe(true);
    expect(wantedOnDevice(event({ severity: 'warning' }), 'problems')).toBe(true);
    expect(wantedOnDevice(resolved, 'problems')).toBe(true);
    expect(wantedOnDevice(event({ severity: 'info' }), 'problems')).toBe(false);
  });

  it('errors only leaves out warnings and recoveries', () => {
    expect(wantedOnDevice(event({ severity: 'warning' }), 'errors')).toBe(false);
    expect(wantedOnDevice({ ...resolved, severity: 'error' }, 'errors')).toBe(false);
    expect(wantedOnDevice(event({ severity: 'error' }), 'errors')).toBe(true);
    expect(wantedOnDevice(event({ severity: 'error' }), 'off')).toBe(false);
  });
});

describe('alertsFor', () => {
  it('names the node and uses the tone of the event', () => {
    expect(alertsFor('jupiter', [event(), resolved], 'problems', true)).toEqual([
      { title: 'jupiter: gitea crashed (exit 1)', body: 'Exited with code 1.', tone: 'error' },
      { title: 'jupiter: gitea is healthy again', body: 'Exited with code 1.', tone: 'success' },
    ]);
  });

  it('turns a burst into one summary', () => {
    const burst = [1, 2, 3, 4].map((id) => event({ id, severity: 'warning' }));
    expect(alertsFor('jupiter', burst, 'problems', false)).toEqual([
      { title: 'jupiter: 4 new events', body: 'See the Events page.', tone: 'warning' },
    ]);
  });

  it('leaves wake results to their own toast while focused', () => {
    const didNotWake = event({ category: 'wol', severity: 'warning', title: "desktop didn't wake" });
    expect(alertsFor('jupiter', [didNotWake], 'problems', true)).toEqual([]);
    expect(alertsFor('jupiter', [didNotWake], 'problems', false)).toHaveLength(1);
  });
});

describe('merging nodes', () => {
  const byNode = {
    a: { events: [event({ id: 2, at: 200 }), event({ id: 1, at: 100 })], problems: [problem({ opened_at: 5 })], hasOlder: false },
    b: {
      events: [event({ id: 7, at: 150 })],
      problems: [problem({ key: 'x', severity: 'error', opened_at: 9 })],
      hasOlder: false,
    },
  };

  it('interleaves events newest first', () => {
    expect(mergeEvents(byNode).map((e) => [e.nodeId, e.id])).toEqual([
      ['a', 2],
      ['b', 7],
      ['a', 1],
    ]);
  });

  it('puts errors before warnings, then the oldest first', () => {
    expect(mergeProblems(byNode).map((p) => [p.nodeId, p.severity])).toEqual([
      ['b', 'error'],
      ['a', 'warning'],
    ]);
  });
});

describe('dayLabel', () => {
  const now = new Date(2026, 8, 24, 12, 0);
  const at = (d: Date) => d.getTime() / 1000;
  it('says today and yesterday, then the date', () => {
    expect(dayLabel(at(new Date(2026, 8, 24, 0, 5)), now)).toBe('Today');
    expect(dayLabel(at(new Date(2026, 8, 23, 23, 59)), now)).toBe('Yesterday');
    expect(dayLabel(at(new Date(2026, 8, 20, 9, 0)), now)).not.toMatch(/Today|Yesterday/);
  });
});

describe('categoryOf', () => {
  it('falls back for a category a newer agent sends', () => {
    expect(categoryOf('service').label).toBe('Service');
    expect(categoryOf('quantum' as never).label).toBe('quantum');
  });
});
