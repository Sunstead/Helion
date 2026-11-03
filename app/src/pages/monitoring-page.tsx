import { useMemo } from 'react';
import { ChartLine, TriangleAlert } from 'lucide-react';
import { nodeDisplayName, useNodeStore } from '@/stores/nodes';
import { useMetricHistories, useMetricHistory, useNodeMeta } from '@/api/queries';
import { formatBytes } from '@/lib/node-metrics';
import { usePersistentState } from '@/hooks/use-persistent-state';
import { PageHeader } from '@/components/page-header';
import { NodeSelect } from '@/components/node-select';
import { ALL_NODES, useNodeScope } from '@/hooks/use-node-scope';
import { SegmentedControl } from '@/components/segmented-control';
import { EmptyState, NoNodesState } from '@/components/empty-state';
import { NodeUnavailable } from '@/components/sign-in';
import { SETUP } from '@/lib/setup';
import { CompareChart, MetricChart } from '@/components/metric-chart';
import { mergeSeries, NumericKey, toRows } from '@/lib/metric-series';
import { TopContainers } from '@/components/top-containers';
import { Skeleton } from '@sunstead/ui/components/skeleton';

const RANGES = {
  '1h': 3_600,
  '6h': 21_600,
  '24h': 86_400,
  '7d': 604_800,
  '30d': 2_592_000,
} as const;

type Range = keyof typeof RANGES;

const rate = (v: number) => `${formatBytes(v)}/s`;
const pct = (v: number) => `${Math.round(v)}%`;

const METRICS: { title: string; key: NumericKey; peak: NumericKey; color: string; format: (v: number) => string; max?: number }[] = [
  { title: 'CPU', key: 'cpu_pct', peak: 'cpu_pct_max', color: 'var(--color-cpu)', format: pct, max: 100 },
  { title: 'Memory', key: 'mem_used_bytes', peak: 'mem_used_bytes', color: 'var(--color-ram)', format: formatBytes },
  { title: 'Network in', key: 'net_rx_bps', peak: 'net_rx_bps_max', color: 'var(--color-network)', format: rate },
  { title: 'Network out', key: 'net_tx_bps', peak: 'net_tx_bps_max', color: 'var(--color-network)', format: rate },
  { title: 'Disk read', key: 'disk_read_bps', peak: 'disk_read_bps_max', color: 'var(--color-disk)', format: rate },
  { title: 'Disk write', key: 'disk_write_bps', peak: 'disk_write_bps_max', color: 'var(--color-disk)', format: rate },
];

const GRID = 'grid gap-4 @4xl:grid-cols-2';

function ChartSkeletons() {
  return (
    <div className={GRID}>
      {METRICS.map((m) => (
        <Skeleton key={m.title} className='aspect-[5/2] min-h-40' />
      ))}
    </div>
  );
}

/** Every node's history side by side, a line per node on each metric. */
function AllNodes({ nodeIds, rangeSeconds }: { nodeIds: string[]; rangeSeconds: number }) {
  const nodes = useNodeStore((s) => s.nodes);
  const { series, loading, error } = useMetricHistories(nodeIds, { rangeSeconds, maxPoints: 600 });
  const named = useMemo(
    () =>
      series.map(({ nodeId }) => {
        const n = nodes.find((x) => x.id === nodeId);
        return { id: nodeId, name: n ? nodeDisplayName(n) : nodeId };
      }),
    [series, nodes],
  );
  const charts = useMemo(
    () => METRICS.map((m) => ({ ...m, rows: mergeSeries(series, m.key) })),
    [series],
  );

  if (error) return <EmptyState size='page' icon={TriangleAlert} title='Could not load history' />;
  if (series.length === 0 && loading) return <ChartSkeletons />;
  if (series.length === 0) {
    return <EmptyState size='page' icon={ChartLine} title='No history yet' description='No node is online with history.' />;
  }
  return (
    <div className={GRID}>
      {charts.map((c) => (
        <CompareChart key={c.title} title={c.title} rows={c.rows} nodes={named} format={c.format} max={c.max} />
      ))}
    </div>
  );
}

export function MonitoringPage() {
  const nodeCount = useNodeStore((s) => s.nodes.length);
  const [scope, setScope, candidates] = useNodeScope('monitoring', {
    capability: 'metrics_history',
    allowAll: true,
  });
  const all = scope === ALL_NODES;
  const nodeId = all ? null : scope;
  const [range, setRange] = usePersistentState<Range>('helion-monitoring-range', '1h');
  const meta = useNodeMeta(nodeId);

  const { data, isLoading, error } = useMetricHistory(nodeId, {
    rangeSeconds: RANGES[range] ?? RANGES['1h'],
    maxPoints: 600,
  });

  const charts = useMemo(
    () =>
      METRICS.map((m) => ({ title: m.title, rows: toRows(data, m.key, m.peak), color: m.color, format: m.format, max: m.max })),
    [data],
  );

  const enabled = meta?.capabilities.metrics_history ?? false;

  const body = () => {
    if (nodeCount === 0) return <NoNodesState />;
    if (all) return <AllNodes nodeIds={candidates} rangeSeconds={RANGES[range] ?? RANGES['1h']} />;
    if (!nodeId || (meta?.status === 'online' && !enabled)) {
      return (
        <EmptyState
          size='page'
          icon={ChartLine}
          title='History not enabled'
          description='This agent is not recording metrics.'
          setup={SETUP.history}
        />
      );
    }
    if (nodeId && (meta?.status === 'offline' || meta?.status === 'unauthorized')) {
      return <NodeUnavailable nodeId={nodeId} />;
    }
    if (error) return <EmptyState size='page' icon={TriangleAlert} title='Could not load history' />;
    if (isLoading || !data) return <ChartSkeletons />;
    if (data.ts.length === 0) {
      return (
        <EmptyState
          size='page'
          icon={ChartLine}
          title='No history yet'
          description='Samples appear within two minutes of the agent starting.'
        />
      );
    }
    return (
      <div className='flex flex-col gap-4'>
        <div className={GRID}>
          {charts.map((c) => (
            <MetricChart key={c.title} {...c} />
          ))}
        </div>
        {meta?.capabilities.container_history && (
          <TopContainers nodeId={nodeId} rangeSeconds={RANGES[range] ?? RANGES['1h']} />
        )}
      </div>
    );
  };

  return (
    <>
      <PageHeader
        title='Monitoring'
        actions={
          nodeCount > 0 && (
            <>
              <NodeSelect value={scope} onChange={setScope} candidates={candidates} allowAll />
              <SegmentedControl<Range>
                label='Time range'
                value={range}
                onChange={setRange}
                options={(Object.keys(RANGES) as Range[]).map((r) => ({ value: r, label: r }))}
              />
            </>
          )
        }
      />
      {body()}
    </>
  );
}
