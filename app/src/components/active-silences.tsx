import { useMemo } from 'react';
import { BellOff } from 'lucide-react';
import { useAlertActions } from '@/api/queries';
import { mergeSilences, SilenceRow } from '@/lib/events';
import { useEventsStore } from '@/stores/events';
import { useNodeStore } from '@/stores/nodes';
import { cn } from '@/lib/utils';
import { Button } from '@sunstead/ui/components/button';
import { NodeName } from './node-name';

function until(s: SilenceRow): string {
  if (s.until === null) return 'until someone ends it';
  const end = new Date(s.until * 1000);
  const sameDay = end.toDateString() === new Date().toDateString();
  return `until ${end.toLocaleString(undefined, sameDay ? { hour: '2-digit', minute: '2-digit' } : { weekday: 'short', hour: '2-digit', minute: '2-digit' })}`;
}

/**
 * Silences in force, one line each, with End now for admins. Renders nothing
 * when there are none, so it can sit above any list.
 */
export function ActiveSilences({ className }: { className?: string }) {
  const byNode = useEventsStore((s) => s.byNode);
  const meta = useNodeStore((s) => s.meta);
  const showNode = useNodeStore((s) => s.nodes.length > 1);
  const silences = useMemo(() => mergeSilences(byNode), [byNode]);
  const { endSilence } = useAlertActions();

  if (silences.length === 0) return null;
  return (
    <div data-silences className={cn('divide-y rounded-lg border border-warning/30 bg-warning/5', className)}>
      {silences.map((s) => (
        <div key={`${s.nodeId}:${s.id}`} className='flex items-center gap-3 px-4 py-2'>
          <BellOff className='size-4 shrink-0 text-warning' />
          <div className='min-w-0 flex-1 text-sm'>
            <p className='wrap-anywhere'>
              Notifications for {s.scope === 'service' ? s.target : 'this node'} are held {until(s)}
            </p>
            <p className='flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground'>
              <span className='truncate'>by {s.created_by}</span>
              {showNode && <span aria-hidden='true'>on</span>}
              {showNode && <NodeName nodeId={s.nodeId} />}
            </p>
          </div>
          {meta[s.nodeId]?.capabilities.alert_actions && (
            <Button variant='ghost' size='sm' onClick={() => void endSilence(s.nodeId, s.id).catch(() => {})}>
              End now
            </Button>
          )}
        </div>
      ))}
    </div>
  );
}
