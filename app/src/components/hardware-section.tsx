import { TriangleAlert } from 'lucide-react';
import { useDiskHealth } from '@/api/queries';
import { DiskHealth } from '@/generated/DiskHealth';
import { TempReading } from '@/generated/TempReading';
import { NO_VALUE } from '@/lib/format';
import { relativeTime } from '@/lib/time';
import { cn } from '@/lib/utils';
import { Section } from './section';

/** Where the agent warns: 5 °C short of critical, else 85 °C. */
function tempTone(t: TempReading): string {
  const warn = t.critical !== null ? t.critical - 5 : 85;
  if (t.critical !== null && t.celsius >= t.critical) return 'text-error';
  return t.celsius >= warn ? 'text-warning' : '';
}

function powerOn(hours: number | null): string {
  if (hours === null) return NO_VALUE;
  const years = hours / 8_766;
  return years >= 1 ? `${years.toFixed(1)} y` : `${Math.round(hours / 24)} d`;
}

/** What to say about a drive's condition, and how loudly. */
function condition(d: DiskHealth): { text: string; tone: string } {
  if (d.passed === false) return { text: 'Failing', tone: 'text-error' };
  if ((d.pending ?? 0) > 0) return { text: `${d.pending} unreadable sectors`, tone: 'text-warning' };
  if ((d.percent_used ?? 0) >= 90) return { text: `${d.percent_used}% worn`, tone: 'text-warning' };
  if ((d.reallocated ?? 0) > 0) return { text: `${d.reallocated} reallocated`, tone: 'text-muted-foreground' };
  if (d.passed === true) return { text: d.percent_used !== null ? `Healthy, ${d.percent_used}% worn` : 'Healthy', tone: 'text-success' };
  return { text: NO_VALUE, tone: 'text-muted-foreground' };
}

/**
 * Temperatures from the agent's own sensors and drive health from the host's
 * S.M.A.R.T. file. Each part shows only when the node has it; nothing at all
 * when it has neither.
 */
export function HardwareSection({ nodeId, temps }: { nodeId: string; temps: TempReading[] }) {
  const { data: health } = useDiskHealth(nodeId);
  const drives = health?.disks ?? [];
  // "tdie2" before "tdie10".
  const sensors = [...temps].sort((a, b) => a.label.localeCompare(b.label, undefined, { numeric: true }));
  if (temps.length === 0 && !health) return null;

  return (
    <Section title='Hardware' contentClassName='divide-y'>
      {temps.length > 0 && (
        <div className='grid grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] gap-x-6 gap-y-2 px-4 py-3'>
          {sensors.map((t) => (
            <div key={t.label} className='flex min-w-0 items-baseline justify-between gap-3 text-xs'>
              <span className='truncate text-muted-foreground' title={t.label}>
                {t.label}
              </span>
              <span className={cn('shrink-0 tabular-nums', tempTone(t))}>{Math.round(t.celsius)} °C</span>
            </div>
          ))}
        </div>
      )}
      {health?.stale && (
        <p className='flex items-center gap-2 px-4 py-2.5 text-xs text-warning'>
          <TriangleAlert className='size-3.5 shrink-0' />
          {health.generated_at
            ? `Drive health was last read ${relativeTime(health.generated_at) ?? 'a while ago'}. The host's timer may have stopped.`
            : 'The host has not written drive health yet.'}
        </p>
      )}
      {drives.map((d) => {
        const c = condition(d);
        return (
          <div
            key={d.serial ?? d.device}
            data-drive
            className='grid grid-cols-[1fr_auto] items-center gap-x-4 gap-y-1 px-4 py-3 @2xl:grid-cols-[minmax(0,1fr)_10rem_4rem_4rem]'
          >
            <div className='min-w-0'>
              <p className='flex min-w-0 items-center gap-2 text-sm'>
                <span className='selectable truncate font-mono text-xs'>{d.device}</span>
                {d.kind && <span className='label-hud text-2xs text-muted-foreground'>{d.kind}</span>}
              </p>
              <p className='truncate text-xs text-muted-foreground'>{d.model ?? NO_VALUE}</p>
            </div>
            <span className={cn('text-right text-xs @2xl:text-left', c.tone)}>{c.text}</span>
            <span className='text-xs tabular-nums text-muted-foreground @2xl:text-right'>
              {d.temp_c !== null ? `${Math.round(d.temp_c)} °C` : NO_VALUE}
            </span>
            <span className='text-right text-xs tabular-nums text-muted-foreground' title='Power-on time'>
              {powerOn(d.power_on_hours)}
            </span>
          </div>
        );
      })}
    </Section>
  );
}
