import { afterEach, describe, expect, it } from 'vitest';
import { useEventsStore } from './events';
import { event, problem } from '@/test/fixtures';

const store = () => useEventsStore.getState();

describe('events store', () => {
  afterEach(() => store().removeNode('n1'));

  it('keeps events newest first as polls add to them', () => {
    store().apply('n1', { events: [event({ id: 1 }), event({ id: 2 })], problems: [], silences: [], initial: true, more: true });
    store().apply('n1', { events: [event({ id: 2 }), event({ id: 3 })], problems: [], silences: [], initial: false, more: false });
    const n = store().byNode.n1;
    expect(n.events.map((e) => e.id)).toEqual([3, 2, 1]);
    expect(n.hasOlder).toBe(true);
  });

  it("doesn't change state when a poll finds nothing new", () => {
    store().apply('n1', { events: [event()], problems: [problem()], silences: [], initial: true, more: false });
    const before = store().byNode;
    store().apply('n1', { events: [], problems: [problem()], silences: [], initial: false, more: false });
    expect(store().byNode).toBe(before);
  });

  it('replaces problems when they change', () => {
    store().apply('n1', { events: [], problems: [problem()], silences: [], initial: true, more: false });
    store().apply('n1', { events: [], problems: [], silences: [], initial: false, more: false });
    expect(store().byNode.n1.problems).toEqual([]);
  });

  it('appends an older page after what it has', () => {
    store().apply('n1', { events: [event({ id: 5 })], problems: [], silences: [], initial: true, more: true });
    store().addOlder('n1', [event({ id: 4 }), event({ id: 3 })], false);
    const n = store().byNode.n1;
    expect(n.events.map((e) => e.id)).toEqual([5, 4, 3]);
    expect(n.hasOlder).toBe(false);
  });
});
