import { useEffect, useSyncExternalStore } from 'react';
import { getConnection } from '@/stores/nodes';
import type { IconRef } from '@/lib/services';

/**
 * Icons the agents hold, as blob URLs. The request needs the node's token,
 * which an `<img src>` can't send, so each is fetched once through the node's
 * client and kept here, outside React state, like the connections.
 *
 * `undefined` is loading, `null` is none (no connection, a 404, a failure).
 * A failure is retried the next time something asks after `RETRY_MS`.
 */
type Entry = { url: string | null; at: number };

const RETRY_MS = 60_000;
const entries = new Map<string, Entry | 'loading'>();
const listeners = new Set<() => void>();

const keyOf = (ref: IconRef, scheme: string) => `${ref.nodeId}\n${ref.name}\n${scheme}`;

function notify() {
  for (const fn of listeners) fn();
}

function load(ref: IconRef, scheme: 'dark' | 'light') {
  const key = keyOf(ref, scheme);
  const current = entries.get(key);
  if (current === 'loading') return;
  if (current && (current.url || Date.now() - current.at < RETRY_MS)) return;

  const conn = getConnection(ref.nodeId);
  if (!conn) return;
  entries.set(key, 'loading');
  conn.client
    .getIcon(ref.name, scheme)
    .then((blob) => entries.set(key, { url: URL.createObjectURL(blob), at: Date.now() }))
    .catch(() => entries.set(key, { url: null, at: Date.now() }))
    .finally(notify);
}

function peek(ref: IconRef | null, scheme: string): string | null | undefined {
  if (!ref) return null;
  const entry = entries.get(keyOf(ref, scheme));
  return entry === 'loading' || !entry ? undefined : entry.url;
}

/** The blob URL of a service's icon on this scheme, once the agent has sent it. */
export function useAgentIcon(ref: IconRef | null, scheme: 'dark' | 'light'): string | null {
  const url = useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => peek(ref, scheme),
  );
  const nodeId = ref?.nodeId;
  const name = ref?.name;
  useEffect(() => {
    if (nodeId && name) load({ nodeId, name }, scheme);
  }, [nodeId, name, scheme]);
  return url ?? null;
}

/** For tests. */
export function resetAgentIcons() {
  for (const entry of entries.values()) {
    if (entry !== 'loading' && entry.url) URL.revokeObjectURL(entry.url);
  }
  entries.clear();
}
