import { useMemo } from 'react';
import { Link } from '@tanstack/react-router';
import { Box, ChevronLeft, ServerOff } from 'lucide-react';
import { useHostInfo, useNodeMeta } from '@/api/queries';
import { useNodeName, useNodeStore } from '@/stores/nodes';
import { useContainersStore } from '@/stores/containers';
import { useAwaiting } from '@/hooks/use-awaiting';
import {
  formatBytes,
  formatDiskRead,
  formatDiskWrite,
  getCpuPct,
  getMemUsagePct,
  getNetRxMbps,
  getNetTxMbps,
  getSwapUsagePct,
} from '@/lib/node-metrics';
import { secondsToDuration } from '@/lib/time';
import { PageHeader } from '@/components/page-header';
import { Section } from '@/components/section';
import { NodeAvatar } from '@/components/node-planet';
import { NodeStatusBadge } from '@/components/node-status-badge';
import NodeOptionsDropdown from '@/components/node-options-dropdown';
import HardwareStatDisplay from '@/components/hardware-stat-display';
import { LiveValue } from '@/components/live-value';
import { EmptyState } from '@/components/empty-state';
import { NodeRecoveryButton } from '@/components/sign-in';
import { DataTable } from '@/components/data-table';
import { DockerDownNote } from '@/components/docker-down-note';
import { containerColumns, ContainerRow } from '@/components/container-columns';
import { buttonVariants } from '@sunstead/ui/components/button';
import { cn } from '@/lib/utils';
import { Hint } from '@/components/hint';
import { Progress } from '@sunstead/ui/components/progress';
import { Skeleton } from '@sunstead/ui/components/skeleton';
import { PeersSection } from '@/components/peers-section';
import { HardwareSection } from '@/components/hardware-section';

function Spec({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className='min-w-0'>
      <p className='label-hud text-2xs text-muted-foreground'>{label}</p>
      <div className='truncate text-sm'>{children}</div>
    </div>
  );
}

function CoreGrid({ nodeId }: { nodeId: string }) {
  const { data: host } = useHostInfo(nodeId);
  if (!host?.cpu_per_core.length) return null;
  return (
    <div className='flex flex-wrap gap-1'>
      {host.cpu_per_core.map((pct, i) => (
        <div
          key={i}
          title={`Core ${i}: ${Math.round(pct)}%`}
          className='flex h-8 w-2.5 items-end overflow-hidden rounded-sm bg-muted'
        >
          <div className='w-full bg-cpu transition-[height] duration-500' style={{ height: `${Math.min(pct, 100)}%` }} />
        </div>
      ))}
    </div>
  );
}

/** The bar's colour by how full the disk is (classes for the progress indicator inside). */
const usageTone = (pct: number) =>
  pct > 90
    ? '[&_[data-slot=progress-indicator]]:bg-error'
    : pct > 75
      ? '[&_[data-slot=progress-indicator]]:bg-warning'
      : '[&_[data-slot=progress-indicator]]:bg-success';

export function NodeDetailPage({ nodeId }: { nodeId: string }) {
  const exists = useNodeStore((s) => s.nodes.some((n) => n.id === nodeId));
  const name = useNodeName(nodeId);
  const meta = useNodeMeta(nodeId);
  const { data: host } = useHostInfo(nodeId);
  const raw = useContainersStore((s) => s.nodeContainers[nodeId]);
  const awaitingContainers = useAwaiting(useContainersStore((s) => s.nodeContainers), nodeId);
  const containers = useMemo<ContainerRow[]>(
    () => (raw ?? []).map((c) => ({ ...c, nodeId })).sort((a, b) => a.name.localeCompare(b.name)),
    [raw, nodeId],
  );
  const columns = useMemo(() => containerColumns(false), []);

  if (!exists) {
    return (
      <>
        <PageHeader title='Node' />
        <EmptyState
          size='page'
          icon={ServerOff}
          title='Node not found'
          action={
            <Link to='/nodes' className={buttonVariants({ variant: 'outline' })}>
              All nodes
            </Link>
          }
        />
      </>
    );
  }

  return (
    <>
      <PageHeader
        title={name ?? 'Node'}
        leading={
          <Hint label='All nodes'>
            <Link to='/nodes' aria-label='All nodes' className={buttonVariants({ variant: 'ghost', size: 'icon', className: '-ml-2' })}>
              <ChevronLeft />
            </Link>
          </Hint>
        }
        actions={<NodeOptionsDropdown nodeId={nodeId} showDetails={false} />}
      />

      <Section title='Overview' contentClassName='flex items-center gap-5 p-4'>
        <NodeAvatar nodeId={nodeId} size={88} />
        {host ? (
          <div className='grid min-w-0 flex-1 grid-cols-2 gap-x-6 gap-y-3 @3xl:grid-cols-4'>
            <Spec label='Status'>
              <NodeStatusBadge nodeId={nodeId} />
            </Spec>
            {meta?.description && <Spec label='Role'>{meta.description}</Spec>}
            <Spec label='Uptime'>
              <LiveValue nodeId={nodeId} className='tabular-nums' format={(h) => secondsToDuration(h.uptime_secs)} />
            </Spec>
            <Spec label='Hostname'>
              <span className='selectable'>{host.hostname}</span>
            </Spec>
            <Spec label='Agent'>
              <span className='font-mono text-xs'>v{meta?.agentVersion}</span>
            </Spec>
            <Spec label='OS'>{host.os}</Spec>
            <Spec label='Kernel'>
              <span className='font-mono text-xs'>{host.kernel} {host.arch}</span>
            </Spec>
            <Spec label='CPU'>
              {host.cpu_model} ({host.cpu_physical_cores}c / {host.cpu_logical_cores}t)
            </Spec>
            <Spec label='Memory'>{formatBytes(host.mem_total_bytes)}</Spec>
          </div>
        ) : meta?.status === 'connecting' ? (
          <div className='grid flex-1 grid-cols-4 gap-3'>
            {Array.from({ length: 8 }, (_, i) => (
              <Skeleton key={i} className='h-9' />
            ))}
          </div>
        ) : (
          <div className='flex flex-1 items-center justify-between gap-4'>
            <div>
              <NodeStatusBadge nodeId={nodeId} />
              {meta?.error && <p className='pt-1 text-xs text-muted-foreground'>{meta.error}</p>}
            </div>
            <NodeRecoveryButton nodeId={nodeId} />
          </div>
        )}
      </Section>

      <PeersSection nodeId={nodeId} />

      {host && (
        <>
          <div className='grid gap-4 @4xl:grid-cols-2'>
            <Section title='Live' contentClassName='flex flex-col gap-2 p-4'>
              <HardwareStatDisplay nodeId={nodeId} metric='cpu' name='CPU' color='var(--color-cpu)' scale='percent' format={(h) => `${getCpuPct(h)}%`} />
              <HardwareStatDisplay nodeId={nodeId} metric='ram' name='Memory' color='var(--color-ram)' scale='percent' format={(h) => `${getMemUsagePct(h)}%`} />
              <HardwareStatDisplay nodeId={nodeId} metric='netRx' name='Net in' color='var(--color-network)' format={(h) => `${getNetRxMbps(h)} Mbps`} />
              <HardwareStatDisplay nodeId={nodeId} metric='netTx' name='Net out' color='var(--color-network)' format={(h) => `${getNetTxMbps(h)} Mbps`} />
              <HardwareStatDisplay nodeId={nodeId} metric='diskRead' name='Read' color='var(--color-disk)' format={formatDiskRead} />
              <HardwareStatDisplay nodeId={nodeId} metric='diskWrite' name='Write' color='var(--color-disk)' format={formatDiskWrite} />
            </Section>

            <Section title='Processor' contentClassName='space-y-4 p-4'>
              <CoreGrid nodeId={nodeId} />
              <div className='grid grid-cols-3 gap-3'>
                {(['load1', 'load5', 'load15'] as const).map((k, i) => (
                  <Spec key={k} label={`Load ${['1m', '5m', '15m'][i]}`}>
                    <LiveValue nodeId={nodeId} className='tabular-nums' format={(h) => h[k].toFixed(2)} />
                  </Spec>
                ))}
              </div>
              {host.swap_total_bytes > 0 && (
                <Spec label='Swap'>
                  <LiveValue
                    nodeId={nodeId}
                    className='tabular-nums'
                    format={(h) => `${formatBytes(h.swap_used_bytes)} of ${formatBytes(h.swap_total_bytes)} (${getSwapUsagePct(h)}%)`}
                  />
                </Spec>
              )}
            </Section>
          </div>

          <Section title='Filesystems' count={host.disk.length} contentClassName='divide-y'>
            {host.disk.map((d) => {
              const pct = d.total_bytes > 0 ? (d.used_bytes / d.total_bytes) * 100 : 0;
              return (
                <div key={d.mount} className='grid grid-cols-[1fr_auto] items-center gap-x-4 gap-y-1.5 px-4 py-3 @2xl:grid-cols-[12rem_1fr_auto]'>
                  <div className='flex min-w-0 items-center gap-2'>
                    <span className='selectable truncate font-mono text-xs'>{d.label}</span>
                    <span className='label-hud text-2xs text-muted-foreground'>{d.kind}</span>
                  </div>
                  <Progress value={pct} className={cn('col-span-2 @2xl:col-span-1', usageTone(pct))} />
                  <span className='row-start-1 text-right text-xs tabular-nums text-muted-foreground @2xl:row-start-auto'>
                    {formatBytes(d.used_bytes)} of {formatBytes(d.total_bytes)}
                  </span>
                </div>
              );
            })}
          </Section>

          <HardwareSection nodeId={nodeId} temps={host.temps ?? []} />
        </>
      )}

      <DockerDownNote nodeId={nodeId} />
      <DataTable
        columns={columns}
        data={containers}
        getRowId={(r) => r.id}
        loading={awaitingContainers}
        empty={<EmptyState size='inline' icon={Box} title='No containers on this node' />}
      />
    </>
  );
}
