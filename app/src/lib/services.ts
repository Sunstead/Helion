import { ContainerHealth } from '@/generated/ContainerHealth';
import { ContainerInfo } from '@/generated/ContainerInfo';

export type ContainerSummary = {
  id: string;
  name: string;
  state: string;
  /** Docker's health check, when the container has one. */
  health?: ContainerHealth | null;
};

export type ServiceStatus = 'running' | 'partial' | 'stopped';

/** An icon an agent holds: which node to ask, and its name. */
export type IconRef = { nodeId: string; name: string };

export type ServiceInfo = {
  key: string;
  nodeId: string;
  /** The agent's icon for this service (`ContainerInfo.icon`), if it found one. */
  icon: string | null;
  name: string;
  description: string | null;
  url: string | null;
  cpu_pct: number;
  mem_used_bytes: number;
  /**
   * Epoch ms of the earliest running container, or null.
   *
   * Deliberately an absolute instant rather than a precomputed duration: a
   * duration would change on every derivation, so the object identity would
   * differ every tick even when nothing about the service had, and every
   * subscriber would re-render. Format it at the point of display.
   */
  startedAt: number | null;
  containers: ContainerSummary[];
  running: number;
  total: number;
  status: ServiceStatus;
};

/**
 * Label value marking infrastructure (proxy, database) hidden from services.
 */
export const SYSTEM_SERVICE_KEY = 'system';

function formatServiceName(key: string): string {
  return key
    .replace(/[-_]/g, ' ')
    .replace(/(^|\s)\S/g, (char) => char.toUpperCase());
}

/**
 * The icon most of a service's containers share, the first among equals. The
 * agent matches the service key before the image, so an app and its database
 * agree; a service it can't name falls to whichever container matched most.
 */
function commonIcon(group: ContainerInfo[]): string | null {
  const counts = new Map<string, number>();
  for (const c of group) {
    if (c.icon) counts.set(c.icon, (counts.get(c.icon) ?? 0) + 1);
  }
  let best: string | null = null;
  let most = 0;
  for (const [icon, n] of counts) {
    if (n > most) {
      best = icon;
      most = n;
    }
  }
  return best;
}

/** The icon to ask for, from the first node that has one. */
export function iconRef(instances: ServiceInfo[]): IconRef | null {
  for (const s of instances) {
    if (s.icon) return { nodeId: s.nodeId, name: s.icon };
  }
  return null;
}

/**
 * Groups one node's containers into services.
 *
 * Services are declared by the `helion.service` Docker label; a container
 * without one becomes a single-container service under its own name.
 * Derivation is per-node so a poll from one node doesn't recompute every
 * other node's services.
 */
export function deriveNodeServices(
  nodeId: string,
  containers: ContainerInfo[],
): ServiceInfo[] {
  const groups = new Map<string, ContainerInfo[]>();

  for (const c of containers) {
    const key = c.helion_service ?? c.name;
    const group = groups.get(key) ?? [];
    group.push(c);
    groups.set(key, group);
  }

  const services: ServiceInfo[] = [];

  for (const [key, group] of groups) {
    const runningContainers = group.filter((c) => c.state === 'running');
    const total = group.length;

    const startedAt =
      runningContainers
        .map((c) => (c.started_at ? Date.parse(c.started_at) : NaN))
        .filter((t) => Number.isFinite(t))
        .sort((a, b) => a - b)[0] ?? null;

    const unhealthy = runningContainers.some((c) => c.health === 'unhealthy');
    const status: ServiceStatus =
      runningContainers.length === 0
        ? 'stopped'
        : runningContainers.length < total || unhealthy
          ? 'partial'
          : 'running';

    services.push({
      key,
      nodeId,
      icon: commonIcon(group),
      name: formatServiceName(key),
      description:
        group.map((c) => c.helion_service_description).find((d) => d != null) ?? null,
      url: group.map((c) => c.helion_service_url).find((u) => u != null) ?? null,
      cpu_pct: group.reduce((sum, c) => sum + c.cpu_pct, 0),
      mem_used_bytes: group.reduce((sum, c) => sum + c.mem_used_bytes, 0),
      startedAt,
      containers: group.map((c) => ({ id: c.id, name: c.name, state: c.state, health: c.health ?? null })),
      running: runningContainers.length,
      total,
      status,
    });
  }

  services.sort((a, b) => a.name.localeCompare(b.name));
  return services;
}

/** Services worth clicking on, with infrastructure filtered out. */
export function userFacingServices(services: ServiceInfo[]): ServiceInfo[] {
  return services.filter((s) => s.key !== SYSTEM_SERVICE_KEY);
}

/**
 * One service across every node that runs it: Helion itself runs on each
 * node, and later so may a proxy or an exporter. Pages list these, so a
 * service shows once, with a row per node inside it.
 */
export type ServiceGroup = {
  key: string;
  name: string;
  description: string | null;
  /** The first instance's URL; instances may differ (see `urls`). */
  url: string | null;
  /** Distinct URLs across instances, in instance order. */
  urls: string[];
  /** In node order, as the store holds them. */
  instances: ServiceInfo[];
  /** The first instance's agent icon, with the node that has it. */
  icon: IconRef | null;
  /** Running only when every instance is; stopped only when every one is. */
  status: ServiceStatus;
  running: number;
  total: number;
};

function groupStatus(instances: ServiceInfo[]): ServiceStatus {
  if (instances.every((s) => s.status === 'running')) return 'running';
  if (instances.every((s) => s.status === 'stopped')) return 'stopped';
  return 'partial';
}

/** Groups services by their `helion.service` key across nodes, sorted by name. */
export function groupServices(services: ServiceInfo[]): ServiceGroup[] {
  const byKey = new Map<string, ServiceInfo[]>();
  for (const s of services) {
    const list = byKey.get(s.key) ?? [];
    list.push(s);
    byKey.set(s.key, list);
  }

  const groups: ServiceGroup[] = [];
  for (const [key, instances] of byKey) {
    const urls = [...new Set(instances.map((s) => s.url).filter((u): u is string => u != null))];
    groups.push({
      key,
      name: instances[0].name,
      description: instances.map((s) => s.description).find((d) => d != null) ?? null,
      url: urls[0] ?? null,
      urls,
      instances,
      icon: iconRef(instances),
      status: groupStatus(instances),
      running: instances.reduce((sum, s) => sum + s.running, 0),
      total: instances.reduce((sum, s) => sum + s.total, 0),
    });
  }

  groups.sort((a, b) => a.name.localeCompare(b.name));
  return groups;
}

/** Groups worth clicking on, with infrastructure filtered out. */
export function userFacingGroups(groups: ServiceGroup[]): ServiceGroup[] {
  return groups.filter((g) => g.key !== SYSTEM_SERVICE_KEY);
}
