import { describe, expect, it } from 'vitest';
import { ContainerInfo } from '@/generated/ContainerInfo';
import { deriveNodeServices, groupServices, iconRef, userFacingGroups, userFacingServices } from './services';

function container(partial: Partial<ContainerInfo>): ContainerInfo {
  return {
    id: 'id',
    name: 'name',
    image: 'image',
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

describe('deriveNodeServices', () => {
  it('groups containers by the helion.service label', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', name: 'immich-server', helion_service: 'immich' }),
      container({ id: 'b', name: 'immich-machine-learning', helion_service: 'immich' }),
      container({ id: 'c', name: 'immich-postgres', helion_service: 'immich' }),
    ]);

    expect(services).toHaveLength(1);
    expect(services[0].key).toBe('immich');
    expect(services[0].total).toBe(3);
    expect(services[0].running).toBe(3);
    expect(services[0].status).toBe('running');
  });

  it('treats an unlabelled container as its own service', () => {
    const services = deriveNodeServices('n1', [container({ name: 'standalone' })]);
    expect(services[0].key).toBe('standalone');
    expect(services[0].name).toBe('Standalone');
  });

  it('title-cases names from hyphenated keys', () => {
    const services = deriveNodeServices('n1', [
      container({ helion_service: 'uptime-kuma' }),
    ]);
    expect(services[0].name).toBe('Uptime Kuma');
  });

  it('reports partial when a running container fails its health check', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', helion_service: 'immich', health: 'unhealthy' }),
      container({ id: 'b', helion_service: 'immich', health: 'healthy' }),
    ]);
    expect(services[0].status).toBe('partial');
    expect(services[0].containers[0].health).toBe('unhealthy');
  });

  it('reports partial when only some containers run', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', helion_service: 'immich', state: 'running' }),
      container({ id: 'b', helion_service: 'immich', state: 'exited' }),
    ]);
    expect(services[0].status).toBe('partial');
    expect(services[0].running).toBe(1);
  });

  it('reports stopped when nothing runs', () => {
    const services = deriveNodeServices('n1', [
      container({ helion_service: 'gitea', state: 'exited' }),
    ]);
    expect(services[0].status).toBe('stopped');
  });

  it('sums cpu and memory across the group', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', helion_service: 'immich', cpu_pct: 1.5, mem_used_bytes: 100 }),
      container({ id: 'b', helion_service: 'immich', cpu_pct: 2.5, mem_used_bytes: 250 }),
    ]);
    expect(services[0].cpu_pct).toBe(4);
    expect(services[0].mem_used_bytes).toBe(350);
  });

  it('takes the earliest start among running containers', () => {
    const services = deriveNodeServices('n1', [
      container({
        id: 'a',
        helion_service: 'immich',
        started_at: '2024-03-01T12:00:00Z',
      }),
      container({
        id: 'b',
        helion_service: 'immich',
        started_at: '2024-03-01T10:00:00Z',
      }),
    ]);
    expect(services[0].startedAt).toBe(Date.parse('2024-03-01T10:00:00Z'));
  });

  it('ignores start times of containers that are not running', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', helion_service: 'x', started_at: '2024-01-01T00:00:00Z', state: 'exited' }),
      container({ id: 'b', helion_service: 'x', started_at: '2024-06-01T00:00:00Z' }),
    ]);
    expect(services[0].startedAt).toBe(Date.parse('2024-06-01T00:00:00Z'));
  });

  it('produces a stable result for identical input', () => {
    // The derivation must not embed `Date.now()`: an identity that changes
    // every tick re-renders every subscriber even when nothing moved.
    const input = [container({ helion_service: 'gitea', started_at: '2024-01-01T00:00:00Z' })];
    expect(deriveNodeServices('n1', input)).toEqual(deriveNodeServices('n1', input));
  });

  it('takes description and url from whichever container carries them', () => {
    const services = deriveNodeServices('n1', [
      container({ id: 'a', helion_service: 'gitea' }),
      container({
        id: 'b',
        helion_service: 'gitea',
        helion_service_description: 'Git hosting',
        helion_service_url: 'gitea.home.example.com',
      }),
    ]);
    expect(services[0].description).toBe('Git hosting');
    expect(services[0].url).toBe('gitea.home.example.com');
  });
});

describe('service icons', () => {
  it('takes the icon most containers of a service share', () => {
    const [service] = deriveNodeServices('n1', [
      container({ id: 'a', name: 'a-db', helion_service: 'media', icon: 'postgres' }),
      container({ id: 'b', name: 'b-app', helion_service: 'media', icon: 'jellyfin' }),
      container({ id: 'c', name: 'c-app', helion_service: 'media', icon: 'jellyfin' }),
    ]);
    expect(service.icon).toBe('jellyfin');
  });

  it('is none when no container has one, as from an older agent', () => {
    const [service] = deriveNodeServices('n1', [container({ id: 'a', helion_service: 'x' })]);
    expect(service.icon).toBeNull();
  });

  it('names the node that has the icon, for a group', () => {
    const a = deriveNodeServices('n1', [container({ helion_service: 'git' })]);
    const b = deriveNodeServices('n2', [container({ helion_service: 'git', icon: 'gitea' })]);
    expect(groupServices([...a, ...b])[0].icon).toEqual({ nodeId: 'n2', name: 'gitea' });
    expect(iconRef(a)).toBeNull();
  });
});

describe('userFacingServices', () => {
  it('hides infrastructure labelled helion.service=system', () => {
    // caddy, postgres, redis and tailscale all carry this in core.yml.
    const services = deriveNodeServices('n1', [
      container({ id: 'a', name: 'caddy', helion_service: 'system' }),
      container({ id: 'b', name: 'postgres', helion_service: 'system' }),
      container({ id: 'c', name: 'gitea', helion_service: 'gitea' }),
    ]);

    expect(services).toHaveLength(2);
    expect(userFacingServices(services).map((s) => s.key)).toEqual(['gitea']);
  });
});

describe('groupServices', () => {
  const helion = (nodeId: string, state = 'running', url: string | null = null) =>
    deriveNodeServices(nodeId, [
      container({ id: `${nodeId}-c`, name: 'helion-agent', helion_service: 'helion', state, helion_service_url: url }),
    ])[0];

  it('merges the same service on two nodes into one group', () => {
    const groups = groupServices([helion('jupiter'), helion('pluto')]);
    expect(groups).toHaveLength(1);
    expect(groups[0].key).toBe('helion');
    expect(groups[0].instances.map((s) => s.nodeId)).toEqual(['jupiter', 'pluto']);
    expect(groups[0].running).toBe(2);
    expect(groups[0].total).toBe(2);
    expect(groups[0].status).toBe('running');
  });

  it('is partial when one node is down and the other runs', () => {
    expect(groupServices([helion('jupiter'), helion('pluto', 'exited')])[0].status).toBe('partial');
  });

  it('is stopped only when every instance is', () => {
    expect(groupServices([helion('jupiter', 'exited'), helion('pluto', 'exited')])[0].status).toBe('stopped');
  });

  it('keeps distinct URLs and takes the first as the url', () => {
    const [g] = groupServices([
      helion('jupiter', 'running', 'helion.home.example.com'),
      helion('pluto'),
      helion('saturn', 'running', 'helion.home.example.com'),
    ]);
    expect(g.url).toBe('helion.home.example.com');
    expect(g.urls).toEqual(['helion.home.example.com']);
  });

  it('sorts groups by name and hides system ones', () => {
    const system = deriveNodeServices('pluto', [container({ name: 'caddy', helion_service: 'system' })]);
    const immich = deriveNodeServices('jupiter', [container({ name: 'immich', helion_service: 'immich' })]);
    const groups = userFacingGroups(groupServices([...immich, helion('pluto'), ...system]));
    expect(groups.map((g) => g.key)).toEqual(['helion', 'immich']);
  });
});
