import { AgentInfo } from '@/generated/AgentInfo';
import { ContainerInfo } from '@/generated/ContainerInfo';
import { HostInfo } from '@/generated/HostInfo';
import { TailnetDevice } from '@/generated/TailnetDevice';
import { TailnetStatus } from '@/generated/TailnetStatus';
import { WolEntry } from '@/generated/WolEntry';
import { Event } from '@/generated/Event';
import { EventsResponse } from '@/generated/EventsResponse';
import { Problem } from '@/generated/Problem';

export function hostInfo(partial: Partial<HostInfo> = {}): HostInfo {
  return {
    name: 'jupiter',
    hostname: 'jupiter.local',
    os: 'Debian GNU/Linux 12',
    kernel: '6.1.0',
    arch: 'x86_64',
    cpu_model: 'AMD Ryzen 7',
    cpu_physical_cores: 8,
    cpu_logical_cores: 16,
    cpu_freq_mhz: 3800,
    mem_total_bytes: 32 * 1024 ** 3,
    swap_total_bytes: 0,
    cpu_pct: 12,
    cpu_per_core: [10, 14],
    mem_used_bytes: 8 * 1024 ** 3,
    swap_used_bytes: 0,
    load1: 0.5,
    load5: 0.4,
    load15: 0.3,
    disk: [],
    nets: [],
    net_rx_bps: 0,
    net_tx_bps: 0,
    uptime_secs: 3600,
    sampled_at: 1_700_000_000,
    seq: 1,
    temps: [],
    ...partial,
  };
}

export const ISSUER = 'https://auth.test/application/o/cosmos/';

export function agentInfo(partial: Partial<AgentInfo> = {}): AgentInfo {
  return {
    agent_version: '0.3.0',
    api_version: 3,
    auth_required: true,
    auth: { kind: 'oidc', issuer: ISSUER, client_id: 'cosmos', scopes: 'openid profile offline_access' },
    node_name: 'jupiter',
    capabilities: {
      host_metrics: true,
      containers: true,
      container_actions: true,
      container_logs: true,
      websocket_logs: true,
      volumes: true,
      volume_actions: true,
      metrics_history: true,
      backups: true,
      tailnet: true,
      wol: true,
      wol_actions: true,
      volume_stream: true,
      all_logs: true,
      events: true,
      notify: true,
      notify_actions: true,
      uptime: true,
      uptime_actions: true,
      backup_actions: true,
      updates: true,
      update_actions: true,
      peers: true,
      container_history: true,
      expected_services: true,
      alert_actions: true,
      event_logs: true,
      disk_health: true,
    },
    ...partial,
  };
}

export function containerInfo(partial: Partial<ContainerInfo> = {}): ContainerInfo {
  return {
    id: 'c1',
    name: 'gitea',
    image: 'gitea/gitea',
    status: 'Up 2 hours',
    state: 'running',
    ports: [],
    started_at: null,
    created_unix: 0,
    restart_count: 0,
    compose_project: null,
    helion_service: null,
    helion_service_description: null,
    helion_service_url: null,
    cpu_pct: 0,
    mem_used_bytes: 0,
    mem_limit_bytes: 0,
    ...partial,
  };
}

export function tailnetDevice(partial: Partial<TailnetDevice> = {}): TailnetDevice {
  const name = partial.name ?? 'desktop';
  return {
    id: 'n1',
    name,
    dns_name: `${name}.tail1234.ts.net`,
    os: 'windows',
    user: 'riley@example.com',
    ips: ['100.64.0.2', 'fd7a:115c:a1e0::2'],
    tags: [],
    is_self: false,
    online: true,
    active: false,
    last_seen: null,
    key_expiry: null,
    key_expired: false,
    connection: { kind: 'idle' },
    exit_node: false,
    rx_bytes: 0,
    tx_bytes: 0,
    ...partial,
  };
}

export function tailnetStatus(partial: Partial<TailnetStatus> = {}): TailnetStatus {
  return {
    tailnet: 'example.github',
    backend_state: 'Running',
    devices: [tailnetDevice()],
    sampled_at: 1_700_000_000,
    ...partial,
  };
}

export function wolEntry(partial: Partial<WolEntry> = {}): WolEntry {
  return {
    target: {
      id: '1',
      name: 'desktop',
      mac: 'aa:bb:cc:dd:ee:ff',
      broadcast: '192.168.1.255',
      port: 9,
      tailnet_device: 'n1',
      probe: null,
    },
    state: 'asleep',
    last_seen: null,
    last_wake: null,
    ...partial,
  };
}

/** A signed-in session for the test issuer, so connections get a token. */
export function signedIn(token = 'tok') {
  return {
    [ISSUER]: {
      issuer: ISSUER,
      accessToken: token,
      expiresAt: Date.now() + 3_600_000,
      name: 'Pat Doe',
      username: 'riley',
      email: 'pat@example.com',
      picture: null,
      groups: ['homelab-admins'],
    },
  };
}

export function event(partial: Partial<Event> = {}): Event {
  return {
    id: 1,
    at: 1_790_000_000,
    category: 'container',
    kind: 'crashed',
    severity: 'error',
    subject: 'gitea',
    title: 'gitea crashed (exit 1)',
    detail: 'Exited with code 1.',
    actor: null,
    service: 'gitea',
    problem: null,
    ...partial,
  };
}

export function problem(partial: Partial<Problem> = {}): Problem {
  return {
    key: 'container:gitea:unhealthy',
    category: 'container',
    kind: 'unhealthy',
    severity: 'warning',
    subject: 'gitea',
    title: 'gitea is unhealthy',
    detail: null,
    service: 'gitea',
    opened_at: 1_790_000_000,
    event_id: 1,
    depends_on: null,
    ...partial,
  };
}

export function eventsResponse(partial: Partial<EventsResponse> = {}): EventsResponse {
  return { events: [], problems: [], latest_id: 0, more: false, silences: [], ...partial };
}
