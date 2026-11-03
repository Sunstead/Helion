import { useCallback, useMemo, useState } from 'react';
import { Hexagon, SearchX } from 'lucide-react';
import { ServiceCard } from '@/components/service-card';
import { NotRunningServices } from '@/components/not-running-services';
import { PageHeader } from '@/components/page-header';
import { SearchInput } from '@/components/search-input';
import { SegmentedControl } from '@/components/segmented-control';
import { EmptyState, NoNodesState } from '@/components/empty-state';
import { DockerDownNote } from '@/components/docker-down-note';
import { SETUP } from '@/lib/setup';
import { useContainersStore } from '@/stores/containers';
import { useAwaiting } from '@/hooks/use-awaiting';
import { CardGridSkeleton } from '@/components/skeletons';
import { nodeDisplayName, useNodeStore } from '@/stores/nodes';
import { ServiceInfo, ServiceStatus, userFacingGroups } from '@/lib/services';
import { matchesQuery } from '@/lib/format';
import { serviceCheck } from '@/lib/uptime';
import { useUptime } from '@/api/queries';

type Filter = 'all' | ServiceStatus;

const SERVICE_GRID = 'grid grid-cols-[repeat(auto-fill,minmax(20rem,1fr))] gap-4';

export function ServicesPage() {
  const nodes = useNodeStore((s) => s.nodes);
  const nodeCount = nodes.length;
  const allGroups = useContainersStore((s) => s.groups);
  const awaiting = useAwaiting(useContainersStore((s) => s.nodeContainers));
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const uptime = useUptime(60_000);

  const services = useMemo(() => userFacingGroups(allGroups), [allGroups]);
  const nodeNames = useMemo(() => new Map(nodes.map((n) => [n.id, nodeDisplayName(n)])), [nodes]);
  const checkFor = useCallback(
    (s: ServiceInfo) => serviceCheck(uptime.items, s.nodeId, s.key),
    [uptime.items],
  );

  const visible = useMemo(
    () =>
      services.filter(
        (s) =>
          (filter === 'all' || s.status === filter) &&
          matchesQuery(
            query,
            s.name,
            s.key,
            s.description,
            ...s.urls,
            ...s.instances.flatMap((i) => [
              nodeNames.get(i.nodeId),
              ...i.containers.map((c) => c.name),
            ]),
          ),
      ),
    [services, query, filter, nodeNames],
  );

  return (
    <>
      <PageHeader
        title='Services'
        count={services.length ? visible.length : undefined}
        actions={
          services.length > 0 && (
            <>
              <SearchInput value={query} onChange={setQuery} placeholder='Search services' />
              <SegmentedControl
                label='Filter by status'
                value={filter}
                onChange={setFilter}
                options={[
                  { value: 'all', label: 'All' },
                  { value: 'running', label: 'Healthy' },
                  { value: 'partial', label: 'Degraded' },
                  { value: 'stopped', label: 'Stopped' },
                ]}
              />
            </>
          )
        }
      />

      {nodeCount > 0 && <DockerDownNote />}
      {nodeCount > 0 && <NotRunningServices showNode={nodeCount > 1} className='mb-4' />}
      {nodeCount === 0 ? (
        <NoNodesState />
      ) : services.length === 0 && awaiting ? (
        <CardGridSkeleton count={6} className={SERVICE_GRID} />
      ) : services.length === 0 ? (
        <EmptyState
          size='page'
          icon={Hexagon}
          title='No services yet'
          description='Label containers to group them into services.'
          setup={SETUP.services}
        />
      ) : visible.length === 0 ? (
        <EmptyState size='page' icon={SearchX} title='No matching services' />
      ) : (
        <div className={SERVICE_GRID}>
          {visible.map((s) => (
            <ServiceCard key={s.key} group={s} showNode={nodeCount > 1} checkFor={checkFor} />
          ))}
        </div>
      )}
    </>
  );
}
