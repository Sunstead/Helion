import {
  Activity,
  Archive,
  Boxes,
  CircleHelp,
  Container,
  HardDrive,
  HeartPulse,
  PackageCheck,
  MousePointerClick,
  Network,
  Power,
  Server,
  type LucideIcon,
} from 'lucide-react';
import { Event } from '@/generated/Event';
import { EventCategory } from '@/generated/EventCategory';
import { Problem } from '@/generated/Problem';
import { Silence } from '@/generated/Silence';
import { NodeEvents } from '@/stores/events';
import { DeviceLevel } from '@/stores/notifications';
import { AlertTone } from './device-alert';

export type EventTone = 'error' | 'warning' | 'info' | 'resolved';

export const CATEGORY: Record<EventCategory, { label: string; icon: LucideIcon }> = {
  container: { label: 'Container', icon: Container },
  backup: { label: 'Backup', icon: Archive },
  disk: { label: 'Disk', icon: HardDrive },
  action: { label: 'Action', icon: MousePointerClick },
  wol: { label: 'Wake-on-LAN', icon: Power },
  agent: { label: 'Agent', icon: Server },
  uptime: { label: 'Uptime', icon: HeartPulse },
  update: { label: 'Update', icon: PackageCheck },
  peer: { label: 'Peers', icon: Network },
  service: { label: 'Service', icon: Boxes },
  host: { label: 'Host', icon: Activity },
};

/**
 * A category's label and icon. A newer agent can send a category this app
 * doesn't know yet; it gets a plain fallback rather than breaking the page.
 */
export function categoryOf(category: EventCategory): { label: string; icon: LucideIcon } {
  return CATEGORY[category] ?? { label: String(category), icon: CircleHelp };
}

export function isResolution(e: Event): boolean {
  return e.problem?.state === 'resolved';
}

export function eventTone(e: Event): EventTone {
  return isResolution(e) ? 'resolved' : e.severity;
}

/** Whether this device should notify for `e`, matching the phone's defaults. */
export function wantedOnDevice(e: Event, level: DeviceLevel): boolean {
  switch (level) {
    case 'off':
      return false;
    case 'errors':
      return e.severity === 'error' && !isResolution(e);
    case 'problems':
      return e.severity !== 'info';
  }
}

export type EventRow = Event & { nodeId: string };
export type ProblemRow = Problem & { nodeId: string };

/** Every node's events in one timeline, newest first. */
export function mergeEvents(byNode: Record<string, NodeEvents>): EventRow[] {
  return Object.entries(byNode)
    .flatMap(([nodeId, n]) => n.events.map((e) => ({ ...e, nodeId })))
    .sort((a, b) => b.at - a.at || b.id - a.id);
}

/**
 * Every node's open problems: unacknowledged first, then errors first, then
 * oldest first. Someone is already on an acknowledged one.
 */
export function mergeProblems(byNode: Record<string, NodeEvents>): ProblemRow[] {
  const rank = { error: 0, warning: 1, info: 2 };
  return Object.entries(byNode)
    .flatMap(([nodeId, n]) => n.problems.map((p) => ({ ...p, nodeId })))
    .sort(
      (a, b) =>
        Number(!!a.acked_by) - Number(!!b.acked_by) ||
        rank[a.severity] - rank[b.severity] ||
        a.opened_at - b.opened_at,
    );
}

export type SilenceRow = Silence & { nodeId: string };

/** Every node's silences in force at `now`, soonest to end first. */
export function mergeSilences(byNode: Record<string, NodeEvents>, now = Date.now() / 1000): SilenceRow[] {
  return Object.entries(byNode)
    .flatMap(([nodeId, n]) => (n.silences ?? []).map((s) => ({ ...s, nodeId })))
    .filter((s) => s.until === null || s.until > now)
    .sort((a, b) => (a.until ?? Infinity) - (b.until ?? Infinity));
}

/** Whether a silence covers events about `service`. */
export function silenceCovers(s: Silence, service: string | null, now: number): boolean {
  if (s.until !== null && s.until <= now) return false;
  return s.scope === 'node' || (service !== null && s.target === service);
}

/**
 * The agent's rule for this device too: nothing about an acknowledged
 * problem except its recovery, and nothing a silence covers.
 */
export function quieted(e: Event, problems: Problem[], silences: Silence[], now = Date.now() / 1000): boolean {
  if (silences.some((s) => silenceCovers(s, e.service, now))) return true;
  if (e.problem?.state === 'opened') {
    return problems.some((p) => p.key === e.problem!.key && !!p.acked_by);
  }
  return false;
}

/** "Today", "Yesterday", or "Mon 22 Sep", for grouping a timeline by day. */
export function dayLabel(unixSecs: number, now = new Date()): string {
  const day = new Date(unixSecs * 1000);
  const startOf = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diff = Math.round((startOf(now) - startOf(day)) / 86_400_000);
  if (diff === 0) return 'Today';
  if (diff === 1) return 'Yesterday';
  return day.toLocaleDateString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    year: day.getFullYear() === now.getFullYear() ? undefined : 'numeric',
  });
}

/** "14:03", in the viewer's zone. */
export function clockTime(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

/** More than this in one poll (say, after being offline) becomes one summary. */
const BURST = 3;

const TONE: Record<EventTone, AlertTone> = {
  error: 'error',
  warning: 'warning',
  info: 'info',
  resolved: 'success',
};

export interface DeviceAlert {
  title: string;
  body: string | null;
  tone: AlertTone;
}

/**
 * What to tell this device about one node's new events. Wake results are left
 * out while the app has focus: WolWatcher already toasts them.
 */
export function alertsFor(node: string, events: Event[], level: DeviceLevel, focused: boolean): DeviceAlert[] {
  const wanted = events.filter((e) => wantedOnDevice(e, level) && !(e.category === 'wol' && focused));
  if (wanted.length > BURST) {
    const tone = wanted.some((e) => e.severity === 'error' && e.problem?.state !== 'resolved') ? 'error' : 'warning';
    return [{ title: `${node}: ${wanted.length} new events`, body: 'See the Events page.', tone }];
  }
  return wanted.map((e) => ({ title: `${node}: ${e.title}`, body: e.detail, tone: TONE[eventTone(e)] }));
}
