import { useMemo } from 'react';
import { Boxes } from 'lucide-react';
import { useContainerHistory } from '@/api/queries';
import { ContainerRank } from '@/generated/ContainerRank';
import { usePersistentState } from '@/hooks/use-persistent-state';
import { containerRows } from '@/lib/metric-series';
import { formatBytes } from '@/lib/node-metrics';
import { CompareChart } from './metric-chart';
import { EmptyState } from './empty-state';
import { Section } from './section';
import { SegmentedControl } from './segmented-control';
import { Skeleton } from '@sunstead/ui/components/skeleton';

const TOP = 8;

const RANKS: Record<ContainerRank, { label: string; key: 'cpu_pct' | 'mem_used_bytes'; format: (v: number) => string }> = {
  cpu: { label: 'CPU', key: 'cpu_pct', format: (v) => `${Math.round(v)}%` },
  mem: { label: 'Memory', key: 'mem_used_bytes', format: formatBytes },
};

/**
 * The containers that used the most over the range, a line each, so a spike
 * on the host charts above can be put down to one of them.
 */
export function TopContainers({ nodeId, rangeSeconds }: { nodeId: string; rangeSeconds: number }) {
  const [rank, setRank] = usePersistentState<ContainerRank>('helion-monitoring-top', 'cpu');
  const by = RANKS[rank] ?? RANKS.cpu;
  const { data, isLoading, error } = useContainerHistory(nodeId, { rangeSeconds, top: rank, limit: TOP });

  const rows = useMemo(() => containerRows(data, by.key), [data, by.key]);
  const lines = useMemo(() => (data?.containers ?? []).map((c) => ({ id: c.name, name: c.name })), [data]);

  const control = (
    <SegmentedControl<ContainerRank>
      label='Rank containers by'
      value={rank}
      onChange={setRank}
      options={(Object.keys(RANKS) as ContainerRank[]).map((r) => ({ value: r, label: RANKS[r].label }))}
    />
  );

  if (isLoading) return <Skeleton className='aspect-[5/2] min-h-40' />;
  if (error || !data || data.containers.length === 0) {
    return (
      <Section title='Top containers' actions={control}>
        <EmptyState
          icon={Boxes}
          title={error ? 'Could not load container history' : 'No container history yet'}
          description={error ? undefined : 'Each container gets a point a minute while it runs.'}
        />
      </Section>
    );
  }
  return (
    <CompareChart
      title='Top containers'
      actions={control}
      rows={rows}
      nodes={lines}
      format={by.format}
      gaps
    />
  );
}
