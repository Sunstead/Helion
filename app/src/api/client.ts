import { AgentInfo } from '@/generated/AgentInfo';
import { ApiError } from '@/generated/ApiError';
import { BackupRequestKind } from '@/generated/BackupRequestKind';
import { BackupsStatus } from '@/generated/BackupsStatus';
import { ContainerActionResult } from '@/generated/ContainerActionResult';
import { ContainersResponse } from '@/generated/ContainersResponse';
import { EventsResponse } from '@/generated/EventsResponse';
import { HostInfo } from '@/generated/HostInfo';
import { LogsResponse } from '@/generated/LogsResponse';
import { MetricSeries } from '@/generated/MetricSeries';
import { MetricStep } from '@/generated/MetricStep';
import { NotifyChannel } from '@/generated/NotifyChannel';
import { NotifyChannelInput } from '@/generated/NotifyChannelInput';
import { NotifyResponse } from '@/generated/NotifyResponse';
import { NotifySettings } from '@/generated/NotifySettings';
import { VolumesResponse } from '@/generated/VolumesResponse';
import { Capabilities } from '@/generated/Capabilities';
import { TailnetStatus } from '@/generated/TailnetStatus';
import { UpdatePolicy } from '@/generated/UpdatePolicy';
import { UpdateRun } from '@/generated/UpdateRun';
import { PeersResponse } from '@/generated/PeersResponse';
import { UpdatesResponse } from '@/generated/UpdatesResponse';
import { UptimeCheckInput } from '@/generated/UptimeCheckInput';
import { UptimeEntry } from '@/generated/UptimeEntry';
import { UptimeResponse } from '@/generated/UptimeResponse';
import { UptimeServiceInput } from '@/generated/UptimeServiceInput';
import { WolEntry } from '@/generated/WolEntry';
import { WolNeighborsResponse } from '@/generated/WolNeighborsResponse';
import { WolResponse } from '@/generated/WolResponse';
import { WolTarget } from '@/generated/WolTarget';
import { WolTargetInput } from '@/generated/WolTargetInput';

/** A typed failure from an agent, carrying the agent's own error code. */
export class AgentRequestError extends Error {
  constructor(
    readonly status: number,
    readonly body: ApiError | null,
  ) {
    super(body?.message ?? `request failed with ${status}`);
    this.name = 'AgentRequestError';
  }

  get code() {
    return this.body?.code ?? null;
  }

  /** 401: the token is missing or wrong. Prompt, don't retry. */
  get isUnauthorized() {
    return this.status === 401;
  }

  /**
   * 501: feature disabled on this agent, so hide it. 503: failing now, retry.
   */
  get isPermanentlyUnavailable() {
    return this.status === 501;
  }
}

/** What a pre-0.2 agent (no /v1/info route) is assumed to support. */
export const LEGACY_CAPABILITIES: Capabilities = {
  host_metrics: true,
  containers: true,
  container_actions: false,
  container_logs: false,
  websocket_logs: false,
  volumes: true,
  volume_actions: false,
  metrics_history: false,
  backups: false,
  tailnet: false,
  wol: false,
  wol_actions: false,
  volume_stream: false,
  all_logs: false,
  events: false,
  notify: false,
  notify_actions: false,
  uptime: false,
  uptime_actions: false,
  backup_actions: false,
  updates: false,
  update_actions: false,
  peers: false,
};

/**
 * What this caller can do here. The agent reports node-level capabilities;
 * a viewer can't act even where the node allows it. Agents before API v2
 * don't report a principal, and their token holder is effectively an admin.
 */
export function effectiveCapabilities(info: AgentInfo): Capabilities {
  if (!info.principal || info.principal.admin) return info.capabilities;
  return {
    ...info.capabilities,
    container_actions: false,
    volume_actions: false,
    wol_actions: false,
    // Channels are admin-only to read, not just to edit.
    notify: false,
    notify_actions: false,
    uptime_actions: false,
    backup_actions: false,
    update_actions: false,
  };
}

export class AgentClient {
  constructor(
    readonly baseUrl: string,
    private token: string | null = null,
  ) {}

  setToken(token: string | null) {
    this.token = token;
  }

  private headers(): HeadersInit {
    return this.token ? { Authorization: `Bearer ${this.token}` } : {};
  }

  /**
   * EventSource and WebSocket cannot set request headers, so streaming URLs
   * carry the token in the query string. The agent accepts either.
   */
  private streamUrl(path: string, params: Record<string, string> = {}): string {
    const url = new URL(this.baseUrl + path);
    for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);
    if (this.token) url.searchParams.set('token', this.token);
    return url.toString();
  }

  private async request<T>(path: string, init: RequestInit = {}): Promise<T> {
    const res = await fetch(this.baseUrl + path, {
      ...init,
      headers: { ...this.headers(), ...(init.headers ?? {}) },
    });

    if (!res.ok) {
      let body: ApiError | null = null;
      try {
        body = (await res.json()) as ApiError;
      } catch {
        // A proxy in front of the agent may return HTML.
      }
      throw new AgentRequestError(res.status, body);
    }

    if (res.status === 204) return undefined as T;
    return res.json() as Promise<T>;
  }

  // --- capability negotiation ---------------------------------------------

  /**
   * Unauthenticated probe. A 404 here means a pre-0.2 agent that predates the
   * route, so we fall back to the original read-only endpoint set rather than
   * failing outright.
   */
  async getInfo(): Promise<AgentInfo> {
    try {
      return await this.request<AgentInfo>('/v1/info');
    } catch (e) {
      if (e instanceof AgentRequestError && e.status === 404) {
        return {
          agent_version: '0.1.x',
          api_version: 0,
          auth_required: false,
          node_name: null,
          capabilities: LEGACY_CAPABILITIES,
        };
      }
      throw e;
    }
  }

  // --- reads ---------------------------------------------------------------

  getHost(): Promise<HostInfo> {
    return this.request<HostInfo>('/v1/host');
  }

  getContainers(): Promise<ContainersResponse> {
    return this.request<ContainersResponse>('/v1/containers');
  }

  getVolumes(): Promise<VolumesResponse> {
    return this.request<VolumesResponse>('/v1/volumes');
  }

  getBackups(): Promise<BackupsStatus> {
    return this.request<BackupsStatus>('/v1/backups');
  }

  getTailnet(): Promise<TailnetStatus> {
    return this.request<TailnetStatus>('/v1/tailnet');
  }

  getUptime(): Promise<UptimeResponse> {
    return this.request<UptimeResponse>('/v1/uptime');
  }

  getPeers(): Promise<PeersResponse> {
    return this.request<PeersResponse>('/v1/peers');
  }

  /** An icon the agent holds (`ContainerInfo.icon`), as the SVG that reads on `theme`. */
  async getIcon(name: string, theme: 'dark' | 'light'): Promise<Blob> {
    const res = await fetch(`${this.baseUrl}/v1/icons/${encodeURIComponent(name)}?theme=${theme}`, {
      headers: this.headers(),
    });
    if (!res.ok) throw new AgentRequestError(res.status, null);
    return res.blob();
  }

  getUpdates(): Promise<UpdatesResponse> {
    return this.request<UpdatesResponse>('/v1/updates');
  }

  getWol(): Promise<WolResponse> {
    return this.request<WolResponse>('/v1/wol');
  }

  /**
   * The event log. `after`: what's new since that id, oldest first. Otherwise
   * the newest page (before `before`, if given), newest first. Always with the
   * problems open now.
   */
  getEvents(opts: { after?: number; before?: number; limit?: number } = {}): Promise<EventsResponse> {
    const params = new URLSearchParams();
    if (opts.after !== undefined) params.set('after', String(opts.after));
    if (opts.before !== undefined) params.set('before', String(opts.before));
    if (opts.limit !== undefined) params.set('limit', String(opts.limit));
    return this.request<EventsResponse>(`/v1/events?${params}`);
  }

  /** Admin only: notification channels (never their secrets) and settings. */
  getNotify(): Promise<NotifyResponse> {
    return this.request<NotifyResponse>('/v1/notify');
  }

  /** Admin only: machines in the host's ARP table, to pick a MAC from. */
  getWolNeighbors(): Promise<WolNeighborsResponse> {
    return this.request<WolNeighborsResponse>('/v1/wol/neighbors');
  }

  getMetrics(opts: {
    from?: number;
    to?: number;
    step?: MetricStep;
    maxPoints?: number;
  }): Promise<MetricSeries> {
    const params = new URLSearchParams();
    if (opts.from !== undefined) params.set('from', String(Math.floor(opts.from)));
    if (opts.to !== undefined) params.set('to', String(Math.floor(opts.to)));
    if (opts.step) params.set('step', opts.step);
    if (opts.maxPoints) params.set('max_points', String(opts.maxPoints));
    return this.request<MetricSeries>(`/v1/metrics?${params}`);
  }

  getLogs(id: string, opts: { tail?: number; since?: number } = {}): Promise<LogsResponse> {
    const params = new URLSearchParams();
    if (opts.tail) params.set('tail', String(opts.tail));
    if (opts.since) params.set('since', String(Math.floor(opts.since)));
    return this.request<LogsResponse>(
      `/v1/containers/${encodeURIComponent(id)}/logs?${params}`,
    );
  }

  /** The newest lines from every running container, oldest first. */
  getAllLogs(opts: { tail?: number } = {}): Promise<LogsResponse> {
    const params = new URLSearchParams();
    if (opts.tail) params.set('tail', String(opts.tail));
    return this.request<LogsResponse>(`/v1/logs?${params}`);
  }

  // --- actions -------------------------------------------------------------

  containerAction(
    id: string,
    action: 'start' | 'stop' | 'restart',
    timeoutSecs?: number,
  ): Promise<ContainerActionResult> {
    return this.request<ContainerActionResult>(
      `/v1/containers/${encodeURIComponent(id)}/${action}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ timeout_secs: timeoutSecs ?? null }),
      },
    );
  }

  removeContainer(
    id: string,
    opts: { force?: boolean; volumes?: boolean } = {},
  ): Promise<ContainerActionResult> {
    const params = new URLSearchParams();
    if (opts.force) params.set('force', 'true');
    if (opts.volumes) params.set('volumes', 'true');
    return this.request<ContainerActionResult>(
      `/v1/containers/${encodeURIComponent(id)}?${params}`,
      { method: 'DELETE' },
    );
  }

  removeVolume(name: string, force = false): Promise<void> {
    const params = new URLSearchParams();
    if (force) params.set('force', 'true');
    return this.request<void>(`/v1/volumes/${encodeURIComponent(name)}?${params}`, {
      method: 'DELETE',
    });
  }

  createWolTarget(input: WolTargetInput): Promise<WolTarget> {
    return this.request<WolTarget>('/v1/wol/targets', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  updateWolTarget(id: string, input: WolTargetInput): Promise<WolTarget> {
    return this.request<WolTarget>(`/v1/wol/targets/${encodeURIComponent(id)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  deleteWolTarget(id: string): Promise<void> {
    return this.request<void>(`/v1/wol/targets/${encodeURIComponent(id)}`, { method: 'DELETE' });
  }

  /** Sends the magic packet; the entry comes back already `waking`. */
  wake(id: string): Promise<WolEntry> {
    return this.request<WolEntry>(`/v1/wol/targets/${encodeURIComponent(id)}/wake`, {
      method: 'POST',
    });
  }

  /** Checks the registries now; resolves with the new list. */
  checkUpdates(): Promise<UpdatesResponse> {
    return this.request<UpdatesResponse>('/v1/updates/check', { method: 'POST' });
  }

  applyUpdate(unit: string, tag: string): Promise<UpdateRun> {
    return this.request<UpdateRun>('/v1/updates/apply', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ unit, tag }),
    });
  }

  rollbackUpdate(unit: string): Promise<UpdateRun> {
    return this.request<UpdateRun>('/v1/updates/rollback', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ unit }),
    });
  }

  setUpdatePolicy(unit: string, policy: UpdatePolicy): Promise<UpdatesResponse> {
    return this.request<UpdatesResponse>('/v1/updates/policy', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ unit, policy }),
    });
  }

  /** Asks the host to back up now or run the restore test. */
  runBackup(kind: BackupRequestKind): Promise<{ id: string }> {
    return this.request<{ id: string }>('/v1/backups/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ kind }),
    });
  }

  /** Answers once the new check has run, with its first result. */
  createUptimeCheck(input: UptimeCheckInput): Promise<UptimeEntry> {
    return this.request<UptimeEntry>('/v1/uptime/checks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  updateUptimeCheck(id: string, input: UptimeCheckInput): Promise<UptimeEntry> {
    return this.request<UptimeEntry>(`/v1/uptime/checks/${encodeURIComponent(id)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  deleteUptimeCheck(id: string): Promise<void> {
    return this.request<void>(`/v1/uptime/checks/${encodeURIComponent(id)}`, { method: 'DELETE' });
  }

  /** A service's own check: on or off, the path, and what counts as up. */
  saveUptimeService(service: string, input: UptimeServiceInput): Promise<UptimeEntry> {
    return this.request<UptimeEntry>(`/v1/uptime/services/${encodeURIComponent(service)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  createNotifyChannel(input: NotifyChannelInput): Promise<NotifyChannel> {
    return this.request<NotifyChannel>('/v1/notify/channels', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  /** Leave `secret` out of `input` to keep the saved one. */
  updateNotifyChannel(id: string, input: NotifyChannelInput): Promise<NotifyChannel> {
    return this.request<NotifyChannel>(`/v1/notify/channels/${encodeURIComponent(id)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
  }

  deleteNotifyChannel(id: string): Promise<void> {
    return this.request<void>(`/v1/notify/channels/${encodeURIComponent(id)}`, { method: 'DELETE' });
  }

  /** Sends a test message now. Rejects with the channel's reason if it fails. */
  testNotifyChannel(id: string): Promise<void> {
    return this.request<void>(`/v1/notify/channels/${encodeURIComponent(id)}/test`, { method: 'POST' });
  }

  saveNotifySettings(settings: NotifySettings): Promise<NotifySettings> {
    return this.request<NotifySettings>('/v1/notify/settings', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(settings),
    });
  }

  // --- streams -------------------------------------------------------------

  hostStreamUrl(): string {
    return this.streamUrl('/v1/host/stream');
  }

  containersStreamUrl(): string {
    return this.streamUrl('/v1/containers/stream');
  }

  volumesStreamUrl(): string {
    return this.streamUrl('/v1/volumes/stream');
  }

  /** Every running container's lines on one socket, tagged with `container`. */
  allLogsSocketUrl(opts: { tail?: number } = {}): string {
    return this.streamUrl('/v1/logs/ws', { tail: String(opts.tail ?? 200) }).replace(/^http/, 'ws');
  }

  /** WebSocket URL for following a container's logs. */
  logsSocketUrl(id: string, opts: { tail?: number } = {}): string {
    const http = this.streamUrl(`/v1/containers/${encodeURIComponent(id)}/logs/ws`, {
      tail: String(opts.tail ?? 500),
    });
    return http.replace(/^http/, 'ws');
  }
}
