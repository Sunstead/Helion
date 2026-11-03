import { create } from 'zustand';
import { EventsUpdate } from '@/api/connection';
import { Event } from '@/generated/Event';
import { Problem } from '@/generated/Problem';
import { Silence } from '@/generated/Silence';

export interface NodeEvents {
  /** Newest first. */
  events: Event[];
  problems: Problem[];
  /** Silences in force on this node. */
  silences: Silence[];
  /** Older events exist on the agent than the oldest here. */
  hasOlder: boolean;
}

/** Enough for the timeline; older pages load on request and are kept. */
const KEEP = 1_000;

interface EventsStore {
  byNode: Record<string, NodeEvents>;
  apply: (nodeId: string, update: EventsUpdate) => void;
  /** An older page, newest first, from `before=`. */
  addOlder: (nodeId: string, older: Event[], more: boolean) => void;
  removeNode: (nodeId: string) => void;
}

const sameProblems = (a: Problem[], b: Problem[]) =>
  a.length === b.length &&
  a.every((p, i) => p.key === b[i].key && p.event_id === b[i].event_id && p.acked_at === b[i].acked_at);

const sameSilences = (a: Silence[], b: Silence[]) =>
  a.length === b.length && a.every((s, i) => s.id === b[i].id && s.until === b[i].until);

export const useEventsStore = create<EventsStore>((set) => ({
  byNode: {},

  apply: (nodeId, update) =>
    set((s) => {
      const prev = s.byNode[nodeId];
      if (update.initial || !prev) {
        const events = [...update.events].reverse();
        return {
          byNode: {
            ...s.byNode,
            [nodeId]: { events, problems: update.problems, silences: update.silences, hasOlder: update.more },
          },
        };
      }
      // Most polls find nothing new: keep the same objects so nothing re-renders.
      const problems = sameProblems(prev.problems, update.problems) ? prev.problems : update.problems;
      const silences = sameSilences(prev.silences, update.silences) ? prev.silences : update.silences;
      if (update.events.length === 0 && problems === prev.problems && silences === prev.silences) return s;

      const known = new Set(prev.events.map((e) => e.id));
      const fresh = [...update.events].reverse().filter((e) => !known.has(e.id));
      const events = fresh.length ? [...fresh, ...prev.events].slice(0, KEEP) : prev.events;
      const hasOlder = prev.hasOlder || events.length < prev.events.length + fresh.length;
      return { byNode: { ...s.byNode, [nodeId]: { events, problems, silences, hasOlder } } };
    }),

  addOlder: (nodeId, older, more) =>
    set((s) => {
      const prev = s.byNode[nodeId];
      if (!prev) return s;
      const known = new Set(prev.events.map((e) => e.id));
      const events = [...prev.events, ...older.filter((e) => !known.has(e.id))];
      return { byNode: { ...s.byNode, [nodeId]: { ...prev, events, hasOlder: more } } };
    }),

  removeNode: (nodeId) =>
    set((s) => {
      const { [nodeId]: _, ...rest } = s.byNode;
      return { byNode: rest };
    }),
}));
