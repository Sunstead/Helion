import { memo } from 'react';
import { ArrowUpRight } from 'lucide-react';
import { ServiceIcon } from '@/lib/service-icons';
import { iconRef, ServiceGroup, ServiceInfo } from '@/lib/services';
import { msToDuration } from '@/lib/time';
import { NO_VALUE, plural } from '@/lib/format';
import {
  formatCpuPercent,
  getServiceStatusDisplay,
  sortContainersByState,
} from '@/lib/service-utils';
import { formatBytes } from '@/lib/node-metrics';
import { displayHost, serviceHref } from '@/lib/agent-url';
import { openExternal } from '@/lib/open-external';
import { useNow } from '@/api/queries';
import { UptimeEntry } from '@/generated/UptimeEntry';
import { ServiceCheckChip } from './uptime-bar';
import { Card } from '@sunstead/ui/components/card';
import { Dot } from './dot';
import { ServiceActionsMenu } from './service-actions-menu';
import { NodeName } from './node-name';

function Metric({ label, value, className }: { label: string; value: string; className?: string }) {
  return (
    <div className='min-w-0'>
      <p className='label-hud text-2xs text-muted-foreground'>{label}</p>
      <p className={`truncate text-sm tabular-nums ${className ?? ''}`}>{value}</p>
    </div>
  );
}

function ContainerDots({ service }: { service: ServiceInfo }) {
  return (
    <span className='flex items-center gap-1' aria-label={`${service.running} of ${service.total} running`}>
      {sortContainersByState(service.containers).map((c) => (
        <Dot
          key={c.id || c.name}
          variant={c.state !== 'running' ? 'disabled' : c.health === 'unhealthy' ? 'warning' : 'success'}
          title={`${c.name}: ${c.state}${c.health ? `, ${c.health}` : ''}`}
          className='size-1.5'
        />
      ))}
    </span>
  );
}

function OpenLink({ url }: { url: string | null }) {
  const href = serviceHref(url);
  if (!href) return null;
  return (
    <button
      type='button'
      onClick={() => void openExternal(href)}
      className='flex min-w-0 items-center gap-1 text-muted-foreground transition-colors hover:text-foreground'
    >
      <span className='truncate'>{displayHost(url)}</span>
      <ArrowUpRight className='size-3 shrink-0' />
    </button>
  );
}

function uptimeOf(service: ServiceInfo, now: number): string {
  return service.startedAt ? msToDuration(Math.max(0, now - service.startedAt)) : NO_VALUE;
}

/** One node's share of a service that runs on several. */
function InstanceRow({
  service,
  check,
  now,
}: {
  service: ServiceInfo;
  check?: UptimeEntry;
  now: number;
}) {
  const status = getServiceStatusDisplay(service.status);
  // Figures never wrap: the node name gives way first, then (in a narrow
  // card) the uptime and the check, so the menu stays inside the padding.
  const figure = 'shrink-0 whitespace-nowrap text-right tabular-nums text-muted-foreground';
  return (
    <div className='flex items-center gap-2 py-2 text-xs'>
      <Dot variant={status.dotVariant} title={status.label} />
      <span className='min-w-0 flex-1 truncate font-medium'>
        <NodeName nodeId={service.nodeId} />
      </span>
      <span className='shrink-0'>
        <ContainerDots service={service} />
      </span>
      {check && (
        <span className='hidden shrink-0 @xs:flex'>
          <ServiceCheckChip check={check} />
        </span>
      )}
      <span className={`hidden @sm:inline ${figure}`} title='Uptime'>
        {uptimeOf(service, now)}
      </span>
      <span className={`w-12 ${figure}`} title='CPU'>
        {formatCpuPercent(service.cpu_pct)}
      </span>
      <span className={`w-14 ${figure}`} title='Memory'>
        {formatBytes(service.mem_used_bytes)}
      </span>
      <ServiceActionsMenu service={service} size='icon-xs' />
    </div>
  );
}

export const ServiceCard = memo(function ServiceCard({
  group,
  showNode,
  checkFor,
}: {
  group: ServiceGroup;
  showNode?: boolean;
  /** An instance's uptime check, when it has one. */
  checkFor: (service: ServiceInfo) => UptimeEntry | undefined;
}) {
  // Uptime is an instant; re-render once a minute to keep it current.
  const now = useNow(60_000);
  const status = getServiceStatusDisplay(group.status);

  if (group.instances.length === 1) {
    const service = group.instances[0];
    const check = checkFor(service);
    return (
      <Card className='gap-4 px-4 py-4'>
        <div className='flex items-start gap-3'>
          <ServiceIcon service={service.key} icon={iconRef([service])} size={32} className='shrink-0' />
          <div className='min-w-0 flex-1'>
            <p className='truncate font-medium leading-5'>{service.name}</p>
            <p className='truncate text-xs text-muted-foreground'>
              {service.description ?? plural(service.total, 'container')}
            </p>
          </div>
          <ServiceActionsMenu service={service} />
        </div>

        <div className='grid grid-cols-3 gap-2'>
          <Metric label='Uptime' value={uptimeOf(service, now)} />
          <Metric label='CPU' value={formatCpuPercent(service.cpu_pct)} />
          <Metric label='Memory' value={formatBytes(service.mem_used_bytes)} />
        </div>

        <div className='flex items-center gap-2 border-t pt-3 text-xs'>
          <span className={`flex items-center gap-1.5 ${status.textClassName}`}>
            <Dot variant={status.dotVariant} />
            {status.label}
          </span>
          <ContainerDots service={service} />
          {check && <ServiceCheckChip check={check} />}
          {showNode && <NodeName nodeId={service.nodeId} />}
          <span className='flex-1' />
          <OpenLink url={service.url} />
        </div>
      </Card>
    );
  }

  return (
    <Card className='gap-3 px-4 py-4'>
      <div className='flex items-start gap-3'>
        <ServiceIcon service={group.key} icon={group.icon} size={32} className='shrink-0' />
        <div className='min-w-0 flex-1'>
          <p className='truncate font-medium leading-5'>{group.name}</p>
          <p className='truncate text-xs text-muted-foreground'>
            {group.description ?? plural(group.total, 'container')}
          </p>
        </div>
        <span className={`flex shrink-0 items-center gap-1.5 text-xs ${status.textClassName}`}>
          <Dot variant={status.dotVariant} />
          {status.label}
        </span>
      </div>

      <div className='@container divide-y border-t'>
        {group.instances.map((s) => (
          <InstanceRow key={s.nodeId} service={s} check={checkFor(s)} now={now} />
        ))}
      </div>

      {group.urls.length > 0 && (
        <div className='flex flex-wrap items-center justify-end gap-x-3 gap-y-1 text-xs'>
          {group.urls.map((url) => (
            <OpenLink key={url} url={url} />
          ))}
        </div>
      )}
    </Card>
  );
});
