import { useMemo } from 'react';
import { ChartLine, TriangleAlert } from 'lucide-react';
import { useContainerHistory } from '@/api/queries';
import { containerChartRows, ContainerKey } from '@/lib/metric-series';
import { formatBytes } from '@/lib/node-metrics';
import { usePersistentState } from '@/hooks/use-persistent-state';
import { MetricChart } from './metric-chart';
import { EmptyState } from './empty-state';
import { SegmentedControl } from './segmented-control';
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@sunstead/ui/components/sheet';
import { Skeleton } from '@sunstead/ui/components/skeleton';

const RANGES = { '1h': 3_600, '24h': 86_400, '7d': 604_800, '30d': 2_592_000 } as const;
type Range = keyof typeof RANGES;

const rate = (v: number) => `${formatBytes(v)}/s`;

const CHARTS: { title: string; key: ContainerKey; peak?: ContainerKey; color: string; format: (v: number) => string }[] = [
  { title: 'CPU', key: 'cpu_pct', peak: 'cpu_pct_max', color: 'var(--color-cpu)', format: (v) => `${Math.round(v)}%` },
  { title: 'Memory', key: 'mem_used_bytes', peak: 'mem_used_bytes_max', color: 'var(--color-ram)', format: formatBytes },
  { title: 'Network in', key: 'net_rx_bps', color: 'var(--color-network)', format: rate },
  { title: 'Network out', key: 'net_tx_bps', color: 'var(--color-network)', format: rate },
  { title: 'Disk read', key: 'disk_read_bps', color: 'var(--color-disk)', format: rate },
  { title: 'Disk write', key: 'disk_write_bps', color: 'var(--color-disk)', format: rate },
];

/** One container's history, opened from its row. History is kept by name, so it spans recreates. */
export function ContainerHistorySheet({
  nodeId,
  name,
  open,
  onOpenChange,
}: {
  nodeId: string;
  name: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [range, setRange] = usePersistentState<Range>('helion-container-history-range', '24h');
  const rangeSeconds = RANGES[range] ?? RANGES['24h'];
  const { data, isLoading, error } = useContainerHistory(open ? nodeId : null, {
    rangeSeconds,
    names: [name],
    // Enough for a day of minutes: one container is a small response.
    maxPoints: 1_500,
  });

  const charts = useMemo(
    () =>
      CHARTS.map((c) => ({ ...c, rows: containerChartRows(data, name, c.key, c.peak) }))
        // A container on the host network has no traffic of its own.
        .filter((c) => c.rows.some((r) => r.value !== null)),
    [data, name],
  );

  const body = () => {
    if (error) return <EmptyState icon={TriangleAlert} title='Could not load history' />;
    if (isLoading || !data) return CHARTS.slice(0, 2).map((c) => <Skeleton key={c.title} className='aspect-[5/2] min-h-40' />);
    if (charts.length === 0) {
      return (
        <EmptyState
          icon={ChartLine}
          title='No history yet'
          description='A container gets a point each minute while it runs.'
        />
      );
    }
    return charts.map((c) => <MetricChart key={c.title} title={c.title} rows={c.rows} color={c.color} format={c.format} />);
  };

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className='w-full gap-0 data-[side=right]:sm:max-w-xl'>
        <SheetHeader className='border-b'>
          <SheetTitle className='truncate pr-8'>{name}</SheetTitle>
          <SheetDescription>Average with the peak shaded behind it.</SheetDescription>
          <div className='pt-2'>
            <SegmentedControl<Range>
              label='Time range'
              value={range}
              onChange={setRange}
              options={(Object.keys(RANGES) as Range[]).map((r) => ({ value: r, label: r }))}
            />
          </div>
        </SheetHeader>
        <div className='flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4 [&>*]:shrink-0'>{body()}</div>
      </SheetContent>
    </Sheet>
  );
}
