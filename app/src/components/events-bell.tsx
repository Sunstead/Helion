import { useMemo, useState } from 'react';
import { Link } from '@tanstack/react-router';
import { Bell, CircleCheck } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@sunstead/ui/components/popover';
import { useEventsStore } from '@/stores/events';
import { useNodeStore } from '@/stores/nodes';
import { useNotificationPrefs } from '@/stores/notifications';
import { mergeEvents, mergeProblems } from '@/lib/events';
import { cn } from '@/lib/utils';
import { Button } from '@sunstead/ui/components/button';
import { EventItem, ProblemItem } from './event-item';
import { Hint } from './hint';
import { ActiveSilences } from './active-silences';

const RECENT = 5;

/**
 * Open problems at a glance, from any page. The count is problems open now;
 * the dot means warnings or errors you haven't seen on the Events page.
 */
export function EventsBell() {
  const byNode = useEventsStore((s) => s.byNode);
  const seen = useNotificationPrefs((s) => s.seen);
  const showNode = useNodeStore((s) => s.nodes.length > 1);
  const [open, setOpen] = useState(false);

  const problems = useMemo(() => mergeProblems(byNode), [byNode]);
  const recent = useMemo(() => mergeEvents(byNode).slice(0, RECENT), [byNode]);
  const unseen = useMemo(
    () =>
      Object.entries(byNode).some(([nodeId, n]) =>
        n.events.some((e) => e.id > (seen[nodeId] ?? 0) && e.severity !== 'info'),
      ),
    [byNode, seen],
  );

  if (Object.keys(byNode).length === 0) return null;

  // Acknowledged ones are listed but not counted: someone is on them.
  const waiting = problems.filter((p) => !p.acked_by);
  const worst = waiting[0]?.severity;
  const label = waiting.length
    ? `${waiting.length} open ${waiting.length === 1 ? 'problem' : 'problems'}`
    : 'No open problems';

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <Hint label={label}>
        <PopoverTrigger
          render={
            <Button
              variant='ghost'
              size={waiting.length > 0 ? 'default' : 'icon'}
              aria-label={label}
              className={cn(
                'relative text-xs tabular-nums',
                worst === 'error'
                  ? 'text-error hover:text-error'
                  : worst === 'warning'
                    ? 'text-warning hover:text-warning'
                    : 'text-muted-foreground',
              )}
            />
          }
        >
          <Bell />
          {waiting.length > 0 && waiting.length}
          {unseen && (
            <span
              data-unseen
              className='absolute top-1.5 right-1.5 size-1.5 rounded-full bg-primary'
              aria-hidden='true'
            />
          )}
        </PopoverTrigger>
      </Hint>
      <PopoverContent align='end' className='w-96 gap-0 p-0'>
        <div className='max-h-[60vh] divide-y overflow-y-auto'>
          <ActiveSilences className='m-2 rounded-md' />
          <h2 className='label-hud px-4 py-2 text-2xs text-muted-foreground'>
            Open problems
          </h2>
          {problems.length === 0 ? (
            <div className='flex items-center gap-2 px-4 py-2.5 text-sm text-muted-foreground'>
              <CircleCheck className='size-4 text-success' />
              Nothing needs attention.
            </div>
          ) : (
            problems.map((p) => (
              <ProblemItem key={`${p.nodeId}:${p.key}`} problem={p} showNode={showNode} />
            ))
          )}
          {recent.length > 0 && (
            <>
              <h2 className='label-hud px-4 py-2 text-2xs text-muted-foreground'>
                Recent
              </h2>
              {recent.map((e) => (
                <EventItem
                  key={`${e.nodeId}:${e.id}`}
                  event={e}
                  showNode={showNode}
                  relative
                />
              ))}
            </>
          )}
        </div>
        <Link
          to='/events'
          onClick={() => setOpen(false)}
          className='border-t px-4 py-2 text-center text-xs text-muted-foreground hover:bg-muted hover:text-foreground'
        >
          All events
        </Link>
      </PopoverContent>
    </Popover>
  );
}
