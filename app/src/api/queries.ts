import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';
import { HostInfo } from '@/generated/HostInfo';
import { ContainerRank } from '@/generated/ContainerRank';
import { ExpectedService } from '@/generated/ExpectedService';
import { SilenceInput } from '@/generated/SilenceInput';
import { MetricStep } from '@/generated/MetricStep';
import { UpdatePolicy } from '@/generated/UpdatePolicy';
import { UpdatesResponse } from '@/generated/UpdatesResponse';
import { UpdateUnit } from '@/generated/UpdateUnit';
import { UptimeCheckInput } from '@/generated/UptimeCheckInput';
import { UptimeEntry } from '@/generated/UptimeEntry';
import { UptimeServiceInput } from '@/generated/UptimeServiceInput';
import { WolEntry } from '@/generated/WolEntry';
import { WolNetwork } from '@/generated/WolNetwork';
import { WolTargetInput } from '@/generated/WolTargetInput';
import { getConnection, useNodeStore } from '@/stores/nodes';
import { Device, mergeTailnets } from '@/lib/tailnet';
import { AgentRequestError } from './client';
import { NodeMeta } from './connection';
import { NotifyChannelInput } from '@/generated/NotifyChannelInput';
import { NotifySettings } from '@/generated/NotifySettings';

/**
 * Connection status, capabilities and agent version. Changes only on transitions.
 */
export function useNodeMeta(nodeId: string | null): NodeMeta | null {
  return useNodeStore((s) => (nodeId ? (s.meta[nodeId] ?? null) : null));
}

/**
 * The latest host sample, as React state.
 *
 * Re-renders once per second. For values inside otherwise static cards, use
 * `<LiveValue>` or `<Sparkline>`, which write through refs instead.
 */
export function useHostInfo(nodeId: string | null): {
  data: HostInfo | null;
  status: NodeMeta['status'];
} {
  const meta = useNodeMeta(nodeId);

  const subscribe = useCallback(
    (onChange: () => void) => {
      const conn = nodeId ? getConnection(nodeId) : null;
      return conn ? conn.onHost(onChange) : () => {};
    },
    [nodeId],
  );

  // `getHost()` returns the same object until a new sample replaces it, so
  // this is a stable snapshot and won't loop.
  const getSnapshot = useCallback(
    () => (nodeId ? (getConnection(nodeId)?.getHost() ?? null) : null),
    [nodeId],
  );

  const data = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return { data, status: meta?.status ?? 'connecting' };
}

/**
 * Subscribes to a node's host stream without causing a re-render.
 *
 * The callback lives in a ref, so inline closures don't resubscribe.
 */
export function useHostSubscription(
  nodeId: string | null,
  onSample: (host: HostInfo) => void,
) {
  const handler = useRef(onSample);

  // Assigned in an effect, not during render.
  useEffect(() => {
    handler.current = onSample;
  }, [onSample]);

  useEffect(() => {
    if (!nodeId) return;
    const conn = getConnection(nodeId);
    if (!conn) return;
    return conn.onHost((host) => handler.current(host));
  }, [nodeId]);
}

/**
 * Historical metrics from the agent's SQLite store.
 *
 * Disabled when the agent lacks the `metrics_history` capability.
 */
export function useMetricHistory(
  nodeId: string | null,
  opts: { rangeSeconds: number; step?: MetricStep; maxPoints?: number },
) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.metrics_history ?? false;

  return useQuery({
    // Keyed by range length; the window is resolved at fetch time.
    queryKey: ['metrics', nodeId, opts.rangeSeconds, opts.step, opts.maxPoints],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      const to = Math.floor(Date.now() / 1000);
      return conn.client.getMetrics({
        from: to - opts.rangeSeconds,
        to,
        step: opts.step,
        maxPoints: opts.maxPoints,
      });
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    staleTime: 15_000,
    refetchInterval: 60_000,
    retry: 1,
  });
}

/**
 * The same history from several nodes at once, for comparing them. Shares
 * `useMetricHistory`'s cache, so switching between one node and all of them
 * doesn't refetch. Nodes that are offline or record no history are left out.
 */
export function useMetricHistories(
  nodeIds: string[],
  opts: { rangeSeconds: number; step?: MetricStep; maxPoints?: number },
) {
  const readyKey = useNodeStore((s) =>
    nodeIds
      .filter((id) => s.meta[id]?.status === 'online' && s.meta[id]?.capabilities.metrics_history)
      .join('|'),
  );
  const ready = readyKey ? readyKey.split('|') : [];

  return useQueries({
    queries: ready.map((nodeId) => ({
      queryKey: ['metrics', nodeId, opts.rangeSeconds, opts.step, opts.maxPoints],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        const to = Math.floor(Date.now() / 1000);
        return conn.client.getMetrics({
          from: to - opts.rangeSeconds,
          to,
          step: opts.step,
          maxPoints: opts.maxPoints,
        });
      },
      staleTime: 15_000,
      refetchInterval: 60_000,
      retry: 1,
    })),
    combine: (results) => ({
      series: results.flatMap((r, i) => (r.data ? [{ nodeId: ready[i], series: r.data }] : [])),
      loading: results.some((r) => r.isLoading),
      error: results.length > 0 && results.every((r) => r.error) ? results[0].error!.message : null,
    }),
  });
}

/**
 * Per-container history from one node: the named containers, else the top
 * `limit` by `top` over the range. Disabled without `container_history`.
 */
export function useContainerHistory(
  nodeId: string | null,
  opts: { rangeSeconds: number; names?: string[]; top?: ContainerRank; limit?: number; maxPoints?: number },
) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.container_history ?? false;
  const names = opts.names?.join(',');

  return useQuery({
    queryKey: ['container-metrics', nodeId, opts.rangeSeconds, names, opts.top, opts.limit, opts.maxPoints],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      const to = Math.floor(Date.now() / 1000);
      return conn.client.getContainerMetrics({
        from: to - opts.rangeSeconds,
        to,
        maxPoints: opts.maxPoints ?? 600,
        names: opts.names,
        top: opts.top,
        limit: opts.limit,
      });
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    staleTime: 15_000,
    refetchInterval: 60_000,
    retry: 1,
  });
}

/** Other agents this node exchanges heartbeats with. */
export function usePeers(nodeId: string | null) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.peers ?? false;
  return useQuery({
    queryKey: ['peers', nodeId],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      return conn.client.getPeers();
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    // Heartbeats are 15 s apart; nothing changes faster.
    refetchInterval: 15_000,
    retry: 1,
  });
}

/** S.M.A.R.T. from the host's file. It changes hourly, so this polls slowly. */
export function useDiskHealth(nodeId: string | null) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.disk_health ?? false;
  return useQuery({
    queryKey: ['disk-health', nodeId],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      return conn.client.getDiskHealth();
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    refetchInterval: 5 * 60_000,
    retry: 1,
  });
}

export function useBackups(nodeId: string | null) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.backups ?? false;

  return useQuery({
    queryKey: ['backups', nodeId],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      return conn.client.getBackups();
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    // Close behind a backup or restore test someone started.
    refetchInterval: (q) =>
      q.state.data?.requests?.some((r) => r.state === 'queued' || r.state === 'running')
        ? 3_000
        : 60_000,
    retry: 1,
  });
}

const RUN_LABEL: Record<'backup' | 'restore_test', string> = {
  backup: 'Backup',
  restore_test: 'Restore test',
};

/** Starts a backup or a restore test on the node's host, with a toast. */
export function useBackupActions(nodeId: string | null) {
  const client = useQueryClient();
  return useCallback(
    async (kind: 'backup' | 'restore_test') => {
      const conn = nodeId ? getConnection(nodeId) : null;
      if (!conn) return;
      try {
        await conn.client.runBackup(kind);
        toast.success(`${RUN_LABEL[kind]} requested`, {
          description: 'The server picks it up in a moment. This page follows it.',
        });
      } catch (e) {
        toast.error(`Could not start the ${RUN_LABEL[kind].toLowerCase()}`, {
          description: e instanceof Error ? e.message : undefined,
        });
        throw e;
      } finally {
        await client.invalidateQueries({ queryKey: ['backups', nodeId] });
      }
    },
    [client, nodeId],
  );
}

const TAILNET_POLL_MS = 15_000;

export interface TailnetView {
  devices: Device[];
  /** Nodes whose agents have `[tailscale]` enabled. */
  reporting: string[];
  loading: boolean;
  /** Set when every reporting agent failed, e.g. tailscaled is unreachable. */
  error: string | null;
}

/**
 * The tailnet, merged from every agent that can see it. Polled rather than
 * streamed: it changes slowly, and each node already holds three of the ~6
 * connections a browser allows per origin.
 */
export function useTailnet(): TailnetView {
  // A joined string, so the query list only changes when the set does.
  const reportingKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.tailnet)
      .map((n) => n.id)
      .join('|'),
  );
  const reporting = reportingKey ? reportingKey.split('|') : [];

  return useQueries({
    queries: reporting.map((nodeId) => ({
      queryKey: ['tailnet', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getTailnet();
      },
      refetchInterval: TAILNET_POLL_MS,
      retry: 1,
    })),
    combine: (results) => {
      const reports = results.flatMap((r, i) => (r.data ? [{ nodeId: reporting[i], status: r.data }] : []));
      const failed = results.find((r) => r.error);
      return {
        devices: mergeTailnets(reports),
        reporting,
        loading: results.some((r) => r.isLoading),
        error: reports.length === 0 && failed?.error ? failed.error.message : null,
      };
    },
  });
}

const WOL_POLL_MS = 10_000;
/** While something is waking, so "awake" shows up within a couple of seconds. */
const WOL_WAKING_POLL_MS = 2_000;

export interface WolItem extends WolEntry {
  /** The node that owns the target and sends its packets. */
  nodeId: string;
}

export interface WolView {
  items: WolItem[];
  /** Broadcast suggestions, per node. */
  networks: Record<string, WolNetwork[]>;
  /** Nodes with Wake-on-LAN enabled. */
  nodes: string[];
  loading: boolean;
  error: string | null;
}

/** Wake-on-LAN targets across every node that has the feature. */
export function useWol(): WolView {
  const nodesKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.wol)
      .map((n) => n.id)
      .join('|'),
  );
  const nodes = nodesKey ? nodesKey.split('|') : [];

  return useQueries({
    queries: nodes.map((nodeId) => ({
      queryKey: ['wol', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getWol();
      },
      refetchInterval: (q: { state: { data?: { targets: WolEntry[] } } }) =>
        q.state.data?.targets.some((t) => t.state === 'waking') ? WOL_WAKING_POLL_MS : WOL_POLL_MS,
      retry: 1,
    })),
    combine: (results) => {
      const items: WolItem[] = [];
      const networks: Record<string, WolNetwork[]> = {};
      results.forEach((r, i) => {
        if (!r.data) return;
        networks[nodes[i]] = r.data.networks;
        for (const t of r.data.targets) items.push({ ...t, nodeId: nodes[i] });
      });
      items.sort((a, b) => a.target.name.localeCompare(b.target.name, undefined, { sensitivity: 'base' }));
      const failed = results.find((r) => r.error);
      return {
        items,
        networks,
        nodes,
        loading: results.some((r) => r.isLoading),
        error: items.length === 0 && failed?.error ? failed.error.message : null,
      };
    },
  });
}

/** Online nodes where you may add, edit and wake machines. */
export function useWolActionNodes(): string[] {
  const key = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.wol_actions)
      .map((n) => n.id)
      .join('|'),
  );
  return useMemo(() => (key ? key.split('|') : []), [key]);
}

/** Wake, add, edit and remove, with toasts. Refreshes the node's list after. */
export function useWolActions() {
  const client = useQueryClient();
  const [pending, setPending] = useState<string | null>(null);

  const refresh = useCallback(
    (nodeId: string) => client.invalidateQueries({ queryKey: ['wol', nodeId] }),
    [client],
  );

  const wake = useCallback(
    async (nodeId: string, id: string, name: string) => {
      const conn = getConnection(nodeId);
      if (!conn) return false;
      setPending(`wake:${nodeId}:${id}`);
      try {
        await conn.client.wake(id);
        toast(`Waking ${name}`, { description: 'Sent the magic packet. Watching for it to come online.' });
        await refresh(nodeId);
        return true;
      } catch (e) {
        toast.error(`Could not wake ${name}`, { description: e instanceof Error ? e.message : undefined });
        return false;
      } finally {
        setPending(null);
      }
    },
    [refresh],
  );

  const save = useCallback(
    async (nodeId: string, input: WolTargetInput, id?: string) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      if (id) await conn.client.updateWolTarget(id, input);
      else await conn.client.createWolTarget(input);
      await refresh(nodeId);
      toast.success(id ? `Saved ${input.name}` : `Added ${input.name}`);
    },
    [refresh],
  );

  const remove = useCallback(
    async (nodeId: string, id: string, name: string) => {
      const conn = getConnection(nodeId);
      if (!conn) return false;
      try {
        await conn.client.deleteWolTarget(id);
        await refresh(nodeId);
        toast.success(`Removed ${name}`);
        return true;
      } catch (e) {
        toast.error(`Could not remove ${name}`, { description: e instanceof Error ? e.message : undefined });
        return false;
      }
    },
    [refresh],
  );

  return { wake, save, remove, pending };
}

const UPDATES_POLL_MS = 60_000;
/** While an update runs, so each step shows up. */
const UPDATES_ACTIVE_POLL_MS = 5_000;

export interface UpdateItem extends UpdateUnit {
  nodeId: string;
}

export interface UpdatesView {
  items: UpdateItem[];
  /** Per node, the rest of its response: errors, whether it can apply, history. */
  byNode: Record<string, UpdatesResponse>;
  nodes: string[];
  loading: boolean;
  error: string | null;
}

const RUNNING_STATES = ['queued', 'dispatched', 'running', 'watching'];

/** Update units across every node that checks for them. */
export function useUpdates(pollMs = UPDATES_POLL_MS): UpdatesView {
  const nodesKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.updates)
      .map((n) => n.id)
      .join('|'),
  );
  const nodes = nodesKey ? nodesKey.split('|') : [];

  return useQueries({
    queries: nodes.map((nodeId) => ({
      queryKey: ['updates', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getUpdates();
      },
      refetchInterval: (q: { state: { data?: UpdatesResponse } }) =>
        q.state.data?.units.some((u) => u.run && RUNNING_STATES.includes(u.run.state))
          ? UPDATES_ACTIVE_POLL_MS
          : pollMs,
      retry: 1,
    })),
    combine: (results) => {
      const items: UpdateItem[] = [];
      const byNode: Record<string, UpdatesResponse> = {};
      results.forEach((r, i) => {
        if (!r.data) return;
        byNode[nodes[i]] = r.data;
        for (const u of r.data.units) items.push({ ...u, nodeId: nodes[i] });
      });
      const failed = results.find((r) => r.error);
      return {
        items,
        byNode,
        nodes,
        loading: results.some((r) => r.isLoading),
        error: items.length === 0 && failed?.error ? failed.error.message : null,
      };
    },
  });
}

/** Check, update, roll back and set policies, with toasts. */
export function useUpdateActions() {
  const client = useQueryClient();
  const refresh = useCallback(
    (nodeId: string) => client.invalidateQueries({ queryKey: ['updates', nodeId] }),
    [client],
  );
  const run = useCallback(
    async <T,>(nodeId: string, what: string, fn: (c: NonNullable<ReturnType<typeof getConnection>>) => Promise<T>) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      try {
        return await fn(conn);
      } catch (e) {
        toast.error(`Could not ${what}`, { description: e instanceof Error ? e.message : undefined });
        throw e;
      } finally {
        await refresh(nodeId);
      }
    },
    [refresh],
  );

  return {
    check: (nodeId: string) => run(nodeId, 'check for updates', (c) => c.client.checkUpdates()),
    apply: async (nodeId: string, unit: UpdateUnit, tag: string) => {
      await run(nodeId, `update ${unit.name}`, (c) => c.client.applyUpdate(unit.id, tag));
      toast.success(`Updating ${unit.name} to ${tag}`, {
        description: 'The update workflow runs on the server. This page follows it.',
      });
    },
    rollback: async (nodeId: string, unit: UpdateUnit) => {
      await run(nodeId, `roll back ${unit.name}`, (c) => c.client.rollbackUpdate(unit.id));
      toast.success(`Rolling back ${unit.name} to ${unit.previous}`);
    },
    policy: (nodeId: string, unit: UpdateUnit, policy: UpdatePolicy) =>
      run(nodeId, `change updates for ${unit.name}`, (c) => c.client.setUpdatePolicy(unit.id, policy)),
  };
}

export interface ExpectedItem extends ExpectedService {
  nodeId: string;
}

/**
 * Services each node expects to be running, from every node that tracks
 * them. Problems reach the app through the event log; this is for the list.
 */
export function useExpectedServices(pollMs = 30_000) {
  const nodesKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.expected_services)
      .map((n) => n.id)
      .join('|'),
  );
  const nodes = nodesKey ? nodesKey.split('|') : [];

  return useQueries({
    queries: nodes.map((nodeId) => ({
      queryKey: ['expected-services', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getExpectedServices();
      },
      refetchInterval: pollMs,
      retry: 1,
    })),
    combine: (results) => {
      const items: ExpectedItem[] = [];
      results.forEach((r, i) => {
        for (const s of r.data?.services ?? []) items.push({ ...s, nodeId: nodes[i] });
      });
      return items;
    },
  });
}

/** Stops a node expecting a service, with toasts. */
export function useForgetService() {
  const client = useQueryClient();
  return useCallback(
    async (nodeId: string, service: string) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      try {
        await conn.client.forgetExpectedService(service);
        toast.success(`Stopped expecting ${service}`, {
          description: 'It is expected again once it has run for 10 minutes.',
        });
      } catch (e) {
        toast.error(`Could not forget ${service}`, { description: e instanceof Error ? e.message : undefined });
        throw e;
      } finally {
        await client.invalidateQueries({ queryKey: ['expected-services', nodeId] });
      }
    },
    [client],
  );
}

/** Acknowledge problems and silence notifications, with toasts. */
export function useAlertActions() {
  const run = useCallback(
    async (nodeId: string, what: string, fn: (c: NonNullable<ReturnType<typeof getConnection>>) => Promise<unknown>, done?: string) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      try {
        await fn(conn);
        if (done) toast.success(done);
      } catch (e) {
        toast.error(`Could not ${what}`, { description: e instanceof Error ? e.message : undefined });
        throw e;
      } finally {
        await conn.refreshEvents();
      }
    },
    [],
  );
  return {
    ack: (nodeId: string, key: string) => run(nodeId, 'acknowledge it', (c) => c.client.ackProblem(key)),
    unack: (nodeId: string, key: string) => run(nodeId, 'clear the acknowledgement', (c) => c.client.unackProblem(key)),
    silence: (nodeId: string, input: SilenceInput, label: string) =>
      run(nodeId, `silence ${label}`, (c) => c.client.silence(input), `Silenced ${label}`),
    endSilence: (nodeId: string, id: number) => run(nodeId, 'end the silence', (c) => c.client.endSilence(id)),
  };
}

/** A crash's last lines, fetched when they're opened. `null` when none were kept. */
export function useEventLogs(nodeId: string, eventId: number, enabled: boolean) {
  return useQuery({
    queryKey: ['event-logs', nodeId, eventId],
    queryFn: async () => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      try {
        return await conn.client.getEventLogs(eventId);
      } catch (e) {
        if (e instanceof AgentRequestError && e.status === 404) return null;
        throw e;
      }
    },
    enabled,
    // Kept once, never changes.
    staleTime: Infinity,
    retry: false,
  });
}

const UPTIME_POLL_MS = 30_000;

export interface UptimeItem extends UptimeEntry {
  /** The node that runs the check. */
  nodeId: string;
}

export interface UptimeView {
  items: UptimeItem[];
  /** Nodes with uptime checks. */
  nodes: string[];
  loading: boolean;
  error: string | null;
}

/**
 * Uptime checks across every node that has them. Problems reach the app
 * through the event log; this is for the figures and heartbeat bars.
 */
export function useUptime(pollMs = UPTIME_POLL_MS): UptimeView {
  const nodesKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.uptime)
      .map((n) => n.id)
      .join('|'),
  );
  const nodes = nodesKey ? nodesKey.split('|') : [];

  return useQueries({
    queries: nodes.map((nodeId) => ({
      queryKey: ['uptime', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getUptime();
      },
      refetchInterval: pollMs,
      retry: 1,
    })),
    combine: (results) => {
      const items: UptimeItem[] = [];
      results.forEach((r, i) => {
        if (!r.data) return;
        for (const c of r.data.checks) items.push({ ...c, nodeId: nodes[i] });
      });
      const failed = results.find((r) => r.error);
      return {
        items,
        nodes,
        loading: results.some((r) => r.isLoading),
        error: items.length === 0 && failed?.error ? failed.error.message : null,
      };
    },
  });
}

/** Add, edit and remove checks, with toasts. Refreshes the node's list after. */
export function useUptimeActions() {
  const client = useQueryClient();

  const refresh = useCallback(
    (nodeId: string) => client.invalidateQueries({ queryKey: ['uptime', nodeId] }),
    [client],
  );

  /** Resolves with the check's first result, so the form can say whether it works. */
  const save = useCallback(
    async (nodeId: string, input: UptimeCheckInput, id?: string) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      const entry = id ? await conn.client.updateUptimeCheck(id, input) : await conn.client.createUptimeCheck(input);
      await refresh(nodeId);
      return entry;
    },
    [refresh],
  );

  const saveService = useCallback(
    async (nodeId: string, service: string, input: UptimeServiceInput) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      const entry = await conn.client.saveUptimeService(service, input);
      await refresh(nodeId);
      return entry;
    },
    [refresh],
  );

  const remove = useCallback(
    async (nodeId: string, id: string, name: string) => {
      const conn = getConnection(nodeId);
      if (!conn) return false;
      try {
        await conn.client.deleteUptimeCheck(id);
        await refresh(nodeId);
        toast.success(`Removed ${name}`);
        return true;
      } catch (e) {
        toast.error(`Could not remove ${name}`, { description: e instanceof Error ? e.message : undefined });
        return false;
      }
    },
    [refresh],
  );

  return { save, saveService, remove };
}

const PAST: Record<ContainerAction, string> = {
  start: 'Started',
  stop: 'Stopped',
  restart: 'Restarted',
  remove: 'Removed',
};

export type ContainerAction = 'start' | 'stop' | 'restart' | 'remove';

/**
 * Container lifecycle actions with toast feedback. The agent's event watcher
 * pushes the new container list within milliseconds, so nothing is refetched.
 */
export function useContainerActions(nodeId: string | null) {
  const [pending, setPending] = useState<string | null>(null);

  const run = useCallback(
    async (
      containerId: string,
      action: ContainerAction,
      label = containerId.slice(0, 12),
      opts: { force?: boolean; volumes?: boolean } = {},
    ) => {
      const conn = nodeId ? getConnection(nodeId) : null;
      if (!conn) return false;

      setPending(`${containerId}:${action}`);
      try {
        if (action === 'remove') await conn.client.removeContainer(containerId, opts);
        else await conn.client.containerAction(containerId, action);
        toast.success(`${PAST[action]} ${label}`);
        return true;
      } catch (e) {
        toast.error(`Could not ${action} ${label}`, {
          description: e instanceof Error ? e.message : undefined,
        });
        return false;
      } finally {
        setPending(null);
      }
    },
    [nodeId],
  );

  /** One action across several containers, sequentially, with one toast. */
  const runMany = useCallback(
    async (containerIds: string[], action: Exclude<ContainerAction, 'remove'>, label: string) => {
      const conn = nodeId ? getConnection(nodeId) : null;
      if (!conn || containerIds.length === 0) return false;

      setPending(`many:${action}`);
      let failed = 0;
      // Sequential: restarting a compose project all at once can trip
      // dependency ordering.
      for (const id of containerIds) {
        try {
          await conn.client.containerAction(id, action);
        } catch {
          failed += 1;
        }
      }
      setPending(null);

      if (failed === 0) toast.success(`${PAST[action]} ${label}`);
      else toast.error(`${failed} of ${containerIds.length} failed to ${action}`);
      return failed === 0;
    },
    [nodeId],
  );

  return { run, runMany, pending };
}

/**
 * Re-renders on a timer, for durations that would otherwise go stale.
 *
 * Uptime strings only change once a minute, so there is no reason to recompute
 * them on every host sample.
 */
export function useTick(intervalMs = 60_000): number {
  return useSyncExternalStore(
    useCallback(
      (onChange) => {
        const id = setInterval(onChange, intervalMs);
        return () => clearInterval(id);
      },
      [intervalMs],
    ),
    () => Math.floor(Date.now() / intervalMs),
  );
}

/** The current time, updated every `intervalMs`, without reading the clock in render. */
export function useNow(intervalMs = 60_000): number {
  return useTick(intervalMs) * intervalMs;
}

/** Channels change rarely; the poll is for their delivery status. */
const NOTIFY_POLL_MS = 30_000;

/** A node's notification channels and settings. Admins only. */
export function useNotify(nodeId: string | null) {
  const meta = useNodeMeta(nodeId);
  const supported = meta?.capabilities.notify ?? false;
  return useQuery({
    queryKey: ['notify', nodeId],
    queryFn: () => {
      const conn = getConnection(nodeId!);
      if (!conn) throw new Error('node is not connected');
      return conn.client.getNotify();
    },
    enabled: !!nodeId && supported && meta?.status === 'online',
    refetchInterval: NOTIFY_POLL_MS,
    retry: 1,
  });
}

/** Every given node's channels and settings, sharing `useNotify`'s cache. */
export function useNotifyAll(nodeIds: string[]) {
  const readyKey = useNodeStore((s) =>
    nodeIds.filter((id) => s.meta[id]?.status === 'online' && s.meta[id]?.capabilities.notify).join('|'),
  );
  const ready = readyKey ? readyKey.split('|') : [];
  return useQueries({
    queries: ready.map((nodeId) => ({
      queryKey: ['notify', nodeId],
      queryFn: () => {
        const conn = getConnection(nodeId);
        if (!conn) throw new Error('node is not connected');
        return conn.client.getNotify();
      },
      refetchInterval: NOTIFY_POLL_MS,
      retry: 1,
    })),
    combine: (results) => ({
      byNode: Object.fromEntries(results.flatMap((r, i) => (r.data ? [[ready[i], r.data] as const] : []))),
      errors: results.flatMap((r, i) => (r.error ? [{ nodeId: ready[i], message: r.error.message }] : [])),
      loading: results.some((r) => r.isLoading),
    }),
  });
}

/** Add, edit, remove and test channels, and save settings, with toasts. */
export function useNotifyActions() {
  const client = useQueryClient();
  const [pending, setPending] = useState<string | null>(null);

  const refresh = useCallback(
    (nodeId: string) => client.invalidateQueries({ queryKey: ['notify', nodeId] }),
    [client],
  );

  const save = useCallback(
    async (nodeId: string, input: NotifyChannelInput, id?: string) => {
      const conn = getConnection(nodeId);
      if (!conn) throw new Error('node is not connected');
      if (id) await conn.client.updateNotifyChannel(id, input);
      else await conn.client.createNotifyChannel(input);
      await refresh(nodeId);
      toast.success(id ? `Saved ${input.name}` : `Added ${input.name}`);
    },
    [refresh],
  );

  const remove = useCallback(
    async (nodeId: string, id: string, name: string) => {
      const conn = getConnection(nodeId);
      if (!conn) return false;
      try {
        await conn.client.deleteNotifyChannel(id);
        await refresh(nodeId);
        toast.success(`Removed ${name}`);
        return true;
      } catch (e) {
        toast.error(`Could not remove ${name}`, { description: e instanceof Error ? e.message : undefined });
        return false;
      }
    },
    [refresh],
  );

  const test = useCallback(
    async (nodeId: string, id: string, name: string) => {
      const conn = getConnection(nodeId);
      if (!conn) return;
      setPending(`test:${nodeId}:${id}`);
      try {
        await conn.client.testNotifyChannel(id);
        toast.success(`Sent a test to ${name}`);
      } catch (e) {
        toast.error(`${name} did not get it`, { description: e instanceof Error ? e.message : undefined });
      } finally {
        setPending(null);
        await refresh(nodeId);
      }
    },
    [refresh],
  );

  const saveSettings = useCallback(
    async (nodeId: string, settings: NotifySettings) => {
      const conn = getConnection(nodeId);
      if (!conn) return;
      try {
        await conn.client.saveNotifySettings(settings);
        await refresh(nodeId);
        toast.success('Saved');
      } catch (e) {
        toast.error('Could not save', { description: e instanceof Error ? e.message : undefined });
      }
    },
    [refresh],
  );

  return { save, remove, test, saveSettings, pending };
}
