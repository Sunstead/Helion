import { AgentClient, AgentRequestError, effectiveCapabilities, LEGACY_CAPABILITIES } from './client';
import { Capabilities } from '@/generated/Capabilities';
import { ContainerInfo } from '@/generated/ContainerInfo';
import { ContainersResponse } from '@/generated/ContainersResponse';
import { Event } from '@/generated/Event';
import { Problem } from '@/generated/Problem';
import { Silence } from '@/generated/Silence';
import { HostInfo } from '@/generated/HostInfo';
import { PrincipalInfo } from '@/generated/PrincipalInfo';
import { AuthInfo } from '@/generated/AuthInfo';
import { VolumeInfo } from '@/generated/VolumeInfo';

export type NodeStatus =
  | 'connecting'
  | 'online'
  | 'offline'
  /** Reachable, but we need to sign in. Retrying on our own won't help. */
  | 'unauthorized';

export type OidcAuthInfo = Extract<AuthInfo, { kind: 'oidc' }>;

/**
 * Where a connection gets its bearer token. `null` means "sign in first"; a
 * rejection means the provider is unreachable, so the connection retries.
 * Injected so the connection stays testable and knows nothing of the
 * browser/desktop split.
 */
export type TokenProvider = (auth: OidcAuthInfo, opts?: { force?: boolean }) => Promise<string | null>;

export interface NodeMeta {
  status: NodeStatus;
  /** Already narrowed to what this caller may do (see effectiveCapabilities). */
  capabilities: Capabilities;
  /** Who the agent says we are. Null for agents before API v2. */
  principal: PrincipalInfo | null;
  /** How to sign in to this agent. Null for agents before API v3. */
  auth: AuthInfo | null;
  agentVersion: string | null;
  /** What the node is for ("Public edge"), from its config. Agent 0.11+. */
  description: string | null;
  apiVersion: number;
  /** Why we're offline, for the UI to show rather than a bare dot. */
  error: string | null;
}

/** What an events poll found. */
export interface EventsUpdate {
  /** New events, oldest first. */
  events: Event[];
  /** Every problem open now. */
  problems: Problem[];
  /** Silences in force now. */
  silences: Silence[];
  /**
   * The first load, or a reload after the log was reset: history, not news,
   * so nothing should notify for it.
   */
  initial: boolean;
  /** With `initial`: whether there are older events than these. */
  more: boolean;
}

type Listener<T> = (value: T) => void;

/** A node's containers, and whether Docker is answering there. */
export interface ContainersUpdate {
  /** When Docker isn't answering, the last list it gave. */
  containers: ContainerInfo[];
  dockerUnavailable: boolean;
}

const BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000];
const LEGACY_CONTAINER_POLL_MS = 5_000;
/** Only for agents without a volume stream (before 0.4). */
const VOLUME_POLL_MS = 30_000;
/** Access tokens last ~10 minutes; keep the one we send fresh. */
const TOKEN_CHECK_MS = 60_000;
/**
 * Polled, not streamed: each node already holds three of the browser's ~6
 * connections per origin, and ten seconds is soon enough for an alert.
 */
const EVENTS_POLL_MS = 10_000;
const EVENTS_FIRST_PAGE = 100;
const EVENTS_PAGE = 500;
/**
 * A host stream that goes this quiet is hung, even if it never closed.
 * Frames come every second by default; slower agents get five of their own
 * gaps before this trips.
 */
const HOST_SILENCE_MS = 20_000;
const WATCHDOG_MS = 5_000;

/**
 * One agent's streams, polling, reconnect and status, for the node's whole
 * lifetime. Components attach listeners; they never open streams themselves.
 *
 * Reconnect is explicit because EventSource only retries dropped connections,
 * not HTTP errors.
 */
export class NodeConnection {
  readonly client: AgentClient;

  private hostSource: EventSource | null = null;
  private containerSource: EventSource | null = null;
  private volumeSource: EventSource | null = null;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private volumeTimer: ReturnType<typeof setInterval> | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private tokenTimer: ReturnType<typeof setInterval> | null = null;
  private eventsTimer: ReturnType<typeof setInterval> | null = null;
  private watchdogTimer: ReturnType<typeof setInterval> | null = null;
  private attempt = 0;
  private stopped = true;

  private meta: NodeMeta = {
    status: 'connecting',
    capabilities: LEGACY_CAPABILITIES,
    principal: null,
    auth: null,
    agentVersion: null,
    description: null,
    apiVersion: 0,
    error: null,
  };

  private lastHost: HostInfo | null = null;
  private lastVolumes: VolumeInfo[] | null = null;
  private lastSeq = -1;
  private lastHostAt = 0;
  private hostGap = 1_000;
  /** Where the next events poll starts. Null until the first load. */
  private lastEventId: number | null = null;
  private eventsInFlight = false;

  private hostListeners = new Set<Listener<HostInfo>>();
  private containerListeners = new Set<Listener<ContainersUpdate>>();
  private lastContainers: ContainerInfo[] | null = null;
  private volumeListeners = new Set<Listener<VolumeInfo[]>>();
  private metaListeners = new Set<Listener<NodeMeta>>();
  private eventListeners = new Set<Listener<EventsUpdate>>();
  /** Fires when the agent restarts, so stale history can be dropped. */
  private resetListeners = new Set<Listener<void>>();

  constructor(
    readonly nodeId: string,
    baseUrl: string,
    private readonly tokens: TokenProvider = async () => null,
  ) {
    this.client = new AgentClient(baseUrl);
  }

  // --- subscriptions --------------------------------------------------------

  onHost(fn: Listener<HostInfo>): () => void {
    this.hostListeners.add(fn);
    // Hand over the most recent sample immediately so a component mounting
    // mid-stream doesn't render empty for up to a second.
    if (this.lastHost) fn(this.lastHost);
    return () => this.hostListeners.delete(fn);
  }

  onContainers(fn: Listener<ContainersUpdate>): () => void {
    this.containerListeners.add(fn);
    return () => this.containerListeners.delete(fn);
  }

  onVolumes(fn: Listener<VolumeInfo[]>): () => void {
    const wasEmpty = this.volumeListeners.size === 0;
    this.volumeListeners.add(fn);
    if (this.lastVolumes) fn(this.lastVolumes);
    // When polling, the first subscriber would otherwise wait out a full
    // interval. A stream sends its current value on open.
    if (wasEmpty && !this.stopped && !this.volumeSource) void this.pollVolumes();
    return () => this.volumeListeners.delete(fn);
  }

  /** No replay: subscribe before start(), as the node store does. */
  onEvents(fn: Listener<EventsUpdate>): () => void {
    this.eventListeners.add(fn);
    return () => this.eventListeners.delete(fn);
  }

  onMeta(fn: Listener<NodeMeta>): () => void {
    this.metaListeners.add(fn);
    fn(this.meta);
    return () => this.metaListeners.delete(fn);
  }

  onReset(fn: Listener<void>): () => void {
    this.resetListeners.add(fn);
    return () => this.resetListeners.delete(fn);
  }

  getMeta(): NodeMeta {
    return this.meta;
  }

  getHost(): HostInfo | null {
    return this.lastHost;
  }

  private setMeta(patch: Partial<NodeMeta>) {
    const next = { ...this.meta, ...patch };
    if (
      next.status === this.meta.status &&
      next.error === this.meta.error &&
      next.agentVersion === this.meta.agentVersion &&
      next.description === this.meta.description &&
      next.capabilities === this.meta.capabilities &&
      next.principal === this.meta.principal &&
      next.auth === this.meta.auth
    ) {
      return;
    }
    this.meta = next;
    for (const fn of this.metaListeners) fn(next);
  }

  // --- lifecycle ------------------------------------------------------------

  /** After a sign-in: try again now rather than waiting to be asked. */
  retryNow() {
    if (this.stopped) return;
    this.restart();
  }

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    void this.connect();
  }

  stop() {
    this.stopped = true;
    this.teardown();
    this.setMeta({ status: 'connecting', error: null });
  }

  private restart() {
    this.teardown();
    this.attempt = 0;
    this.stopped = false;
    void this.connect();
  }

  private teardown() {
    this.hostSource?.close();
    this.containerSource?.close();
    this.volumeSource?.close();
    this.hostSource = null;
    this.containerSource = null;
    this.volumeSource = null;

    for (const t of [
      this.pollTimer,
      this.volumeTimer,
      this.tokenTimer,
      this.eventsTimer,
      this.watchdogTimer,
    ]) {
      if (t) clearInterval(t);
    }
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.pollTimer = null;
    this.volumeTimer = null;
    this.tokenTimer = null;
    this.eventsTimer = null;
    this.watchdogTimer = null;
    this.retryTimer = null;
    // lastEventId survives: after a reconnect, the next poll picks up what
    // happened while we were away.
  }

  private scheduleRetry(reason: string) {
    if (this.stopped || this.retryTimer) return;

    const delay = BACKOFF_MS[Math.min(this.attempt, BACKOFF_MS.length - 1)];
    this.attempt += 1;
    this.setMeta({ status: 'offline', error: reason });

    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      void this.connect();
    }, delay);
  }

  /**
   * Finds out how to sign in, gets a token, then opens the streams.
   *
   * The `getHost` probe tells a 401 from a dead host, which `EventSource`
   * can't report.
   */
  private async connect() {
    if (this.stopped) return;
    this.teardown();
    this.setMeta({ status: 'connecting', error: null });

    let auth: OidcAuthInfo | null = null;
    try {
      let info = await this.client.getInfo();
      if (info.auth?.kind === 'oidc') {
        auth = info.auth;
        const token = await this.tokens(auth);
        if (!token) return this.needSignIn(info.auth);
        this.client.setToken(token);
        // Again, authenticated, to learn who we are here.
        info = await this.client.getInfo();
      } else if (info.auth_required && !info.auth) {
        // A pre-0.3 agent that wants the old shared token.
        this.setMeta({
          status: 'unauthorized',
          auth: null,
          error: 'This agent is older than 0.3 and uses a shared token. Update it to sign in.',
        });
        return;
      }
      this.setMeta({
        capabilities: effectiveCapabilities(info),
        principal: info.principal ?? null,
        auth: info.auth ?? null,
        agentVersion: info.agent_version,
        description: info.node_description ?? null,
        apiVersion: info.api_version,
      });

      await this.probeHost(auth);
    } catch (e) {
      if (e instanceof AgentRequestError && e.isUnauthorized) return this.needSignIn(auth);
      // A provider that's down (say, restarting in the same deploy) is
      // retried like a dead agent, never mistaken for a sign-out.
      this.scheduleRetry(e instanceof Error ? e.message : 'unreachable');
      return;
    }

    if (this.stopped) return;
    if (auth) this.keepTokenFresh(auth);
    this.openHostStream();
    this.openContainerStream();
    this.openVolumeStream();
    this.startEvents();
  }

  /** A 401 may just be an expired token: refresh once before giving up. */
  private async probeHost(auth: OidcAuthInfo | null) {
    try {
      await this.client.getHost();
    } catch (e) {
      if (!(auth && e instanceof AgentRequestError && e.isUnauthorized)) throw e;
      const token = await this.tokens(auth, { force: true });
      if (!token) throw e;
      this.client.setToken(token);
      await this.client.getHost();
    }
  }

  /** Terminal until a sign-in happens; the node store calls retryNow(). */
  private needSignIn(auth: AuthInfo | null) {
    this.setMeta({ status: 'unauthorized', auth, error: 'Sign in to see this node.' });
  }

  /**
   * Requests pick up the new token at once. Open streams keep theirs: the
   * agent checks a stream's token when it opens, and a reconnect after a
   * drop goes through connect() and gets a fresh one.
   */
  private keepTokenFresh(auth: OidcAuthInfo) {
    this.tokenTimer = setInterval(() => {
      this.tokens(auth).then(
        (token) => token && this.client.setToken(token),
        () => {
          /* the provider is down; the next tick tries again */
        },
      );
    }, TOKEN_CHECK_MS);
  }

  private openHostStream() {
    const source = new EventSource(this.client.hostStreamUrl());
    this.hostSource = source;
    this.lastHostAt = Date.now();

    this.watchdogTimer = setInterval(() => {
      if (this.meta.status !== 'online') return;
      const quiet = Date.now() - this.lastHostAt;
      if (quiet > Math.max(HOST_SILENCE_MS, this.hostGap * 5)) {
        this.teardown();
        this.scheduleRetry(`no data for ${Math.round(quiet / 1000)}s`);
      }
    }, WATCHDOG_MS);

    source.onmessage = (e) => {
      let info: HostInfo;
      try {
        info = JSON.parse(e.data) as HostInfo;
      } catch {
        return;
      }

      // A lower seq than last time means the agent restarted; whatever history
      // we hold is from a different process and its rate deltas don't join up.
      if (info.seq < this.lastSeq) {
        for (const fn of this.resetListeners) fn();
      }
      this.lastSeq = info.seq;
      this.lastHost = info;
      const now = Date.now();
      this.hostGap = now - this.lastHostAt;
      this.lastHostAt = now;

      this.attempt = 0;
      this.setMeta({ status: 'online', error: null });
      for (const fn of this.hostListeners) fn(info);
    };

    source.onerror = () => {
      // CONNECTING: the browser retries on its own. CLOSED: it gave up on an
      // HTTP error, so our backoff takes over.
      if (source.readyState === EventSource.CLOSED) {
        this.scheduleRetry('stream closed');
      }
    };
  }

  private openContainerStream() {
    if (this.meta.apiVersion < 1) {
      // Pre-0.2 agents have no container stream; fall back to polling.
      void this.pollContainers();
      this.pollTimer = setInterval(() => void this.pollContainers(), LEGACY_CONTAINER_POLL_MS);
      return;
    }

    const source = new EventSource(this.client.containersStreamUrl());
    this.containerSource = source;

    source.onmessage = (e) => {
      let parsed: ContainersResponse;
      try {
        parsed = JSON.parse(e.data) as ContainersResponse;
      } catch {
        return; // malformed frame; the next one will do
      }
      this.publishContainers(parsed.containers, parsed.docker_unavailable ?? false);
    };

    // Container stream failures don't mark the node offline.
    source.onerror = () => {
      if (source.readyState === EventSource.CLOSED) {
        this.containerSource = null;
        if (!this.pollTimer) {
          this.pollTimer = setInterval(
            () => void this.pollContainers(),
            LEGACY_CONTAINER_POLL_MS,
          );
        }
      }
    };
  }

  private async pollContainers() {
    try {
      const result = await this.client.getContainers();
      this.publishContainers(result.containers, result.docker_unavailable ?? false);
    } catch (e) {
      // The host stream owns the online/offline decision; Docker being
      // down is worth saying, with whatever list we last had.
      if (e instanceof AgentRequestError && e.code === 'docker_unavailable') {
        this.publishContainers(this.lastContainers ?? [], true);
      }
    }
  }

  private publishContainers(containers: ContainerInfo[], dockerUnavailable: boolean) {
    this.lastContainers = containers;
    for (const fn of this.containerListeners) fn({ containers, dockerUnavailable });
  }

  /**
   * Streamed like containers, so a delete from here or the CLI, and a volume
   * freed by a removed container, show up at once. Older agents are polled.
   */
  private openVolumeStream() {
    if (!this.meta.capabilities.volume_stream) return this.startVolumePolling();

    const source = new EventSource(this.client.volumesStreamUrl());
    this.volumeSource = source;

    source.onmessage = (e) => {
      try {
        const parsed = JSON.parse(e.data) as { volumes: VolumeInfo[] };
        this.publishVolumes(parsed.volumes);
      } catch {
        /* malformed frame; the next one will do */
      }
    };

    // Like containers: a failed volume stream doesn't mark the node offline.
    source.onerror = () => {
      if (source.readyState === EventSource.CLOSED) {
        this.volumeSource = null;
        if (!this.volumeTimer) this.startVolumePolling();
      }
    };
  }

  /** Polls the event log now, after an edit, rather than at the next tick. */
  refreshEvents(): Promise<void> {
    return this.meta.capabilities.events ? this.pollEvents() : Promise.resolve();
  }

  private startEvents() {
    if (!this.meta.capabilities.events) return;
    void this.pollEvents();
    this.eventsTimer = setInterval(() => void this.pollEvents(), EVENTS_POLL_MS);
  }

  private async pollEvents(): Promise<void> {
    if (this.eventsInFlight) return;
    this.eventsInFlight = true;
    try {
      if (this.lastEventId === null) {
        const r = await this.client.getEvents({ limit: EVENTS_FIRST_PAGE });
        this.lastEventId = r.latest_id;
        this.publishEvents({
          events: [...r.events].reverse(),
          problems: r.problems,
          silences: r.silences ?? [],
          initial: true,
          more: r.more,
        });
        return;
      }
      const r = await this.client.getEvents({ after: this.lastEventId, limit: EVENTS_PAGE });
      if (r.latest_id < this.lastEventId) {
        // A new log (the state database was replaced): start over.
        this.lastEventId = null;
        this.eventsInFlight = false;
        return this.pollEvents();
      }
      const newest = r.events.at(-1);
      if (newest) this.lastEventId = newest.id;
      this.publishEvents({ events: r.events, problems: r.problems, silences: r.silences ?? [], initial: false, more: false });
    } catch {
      /* events aren't worth changing node status over */
    } finally {
      this.eventsInFlight = false;
    }
  }

  private publishEvents(update: EventsUpdate) {
    for (const fn of this.eventListeners) fn(update);
  }

  private startVolumePolling() {
    void this.pollVolumes();
    this.volumeTimer = setInterval(() => void this.pollVolumes(), VOLUME_POLL_MS);
  }

  private async pollVolumes() {
    if (this.volumeListeners.size === 0) return;
    try {
      const result = await this.client.getVolumes();
      this.publishVolumes(result.volumes);
    } catch {
      /* volumes are not worth changing node status over */
    }
  }

  private publishVolumes(volumes: VolumeInfo[]) {
    this.lastVolumes = volumes;
    for (const fn of this.volumeListeners) fn(volumes);
  }
}
