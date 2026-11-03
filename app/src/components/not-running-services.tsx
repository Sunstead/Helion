import { useState } from 'react';
import { CircleSlash, X } from 'lucide-react';
import { ExpectedItem, useExpectedServices, useForgetService } from '@/api/queries';
import { useNodeStore } from '@/stores/nodes';
import { relativeTime } from '@/lib/time';
import { Button } from '@sunstead/ui/components/button';
import { ConfirmDialog } from './confirm-dialog';
import { NodeName } from './node-name';
import { Section } from './section';

function ago(unixSecs: number): string {
  return relativeTime(new Date(unixSecs * 1000).toISOString()) ?? '';
}

function status(s: ExpectedItem): string {
  if (s.state === 'stopped') {
    return `Stopped by ${s.stopped_by ?? 'someone'} ${s.stopped_at ? ago(s.stopped_at) : ''}`.trim();
  }
  const where = s.present ? 'Its containers are stopped' : 'No containers exist';
  return `${where}. Last running ${ago(s.last_running)}.`;
}

/**
 * Services a node expects that aren't running: gone, stopped outside Helion,
 * or stopped through it. Each can be forgotten, so a retired service stops
 * being expected.
 */
export function NotRunningServices({ showNode, className }: { showNode: boolean; className?: string }) {
  const items = useExpectedServices().filter((s) => s.state !== 'running');
  const meta = useNodeStore((s) => s.meta);
  const canForget = (nodeId: string) => meta[nodeId]?.capabilities.container_actions ?? false;
  const forget = useForgetService();
  const [confirming, setConfirming] = useState<ExpectedItem | null>(null);

  if (items.length === 0) return null;
  return (
    <Section title='Not running' count={items.length} className={className}>
      <ul className='divide-y'>
        {items.map((s) => (
          <li key={`${s.nodeId}:${s.service}`} data-expected-service className='flex items-center gap-3 px-4 py-2.5'>
            <CircleSlash
              className={s.state === 'missing' ? 'size-4 shrink-0 text-warning' : 'size-4 shrink-0 text-muted-foreground'}
            />
            <div className='min-w-0 flex-1'>
              <p className='truncate text-sm'>
                {s.service}
                <span className='ml-2 text-xs text-muted-foreground'>{s.state === 'missing' ? 'Missing' : 'Stopped'}</span>
              </p>
              <p className='text-xs text-muted-foreground'>{status(s)}</p>
              {showNode && (
                <p className='flex min-w-0 text-xs'>
                  <NodeName nodeId={s.nodeId} />
                </p>
              )}
            </div>
            {canForget(s.nodeId) && (
              <Button variant='ghost' size='sm' onClick={() => setConfirming(s)}>
                <X /> Forget
              </Button>
            )}
          </li>
        ))}
      </ul>
      <ConfirmDialog
        open={!!confirming}
        onOpenChange={(open) => !open && setConfirming(null)}
        title={`Forget ${confirming?.service ?? ''}?`}
        description='It will no longer be expected to run, and its problem closes. If it runs for 10 minutes again, it is expected again.'
        confirmLabel='Forget'
        destructive={false}
        onConfirm={async () => {
          if (confirming) await forget(confirming.nodeId, confirming.service);
        }}
      />
    </Section>
  );
}
