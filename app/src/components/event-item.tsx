import { memo } from 'react';
import { CircleCheck } from 'lucide-react';
import { categoryOf, clockTime, EventRow, eventTone, EventTone, ProblemRow } from '@/lib/events';
import { relativeTime } from '@/lib/time';
import { cn } from '@/lib/utils';
import { NodeName } from './node-name';
import { ProblemMenu } from './problem-menu';
import { CrashLines } from './crash-lines';
import { useNodeStore } from '@/stores/nodes';

const TONE: Record<EventTone, string> = {
  error: 'bg-error/10 text-error',
  warning: 'bg-warning/10 text-warning',
  info: 'bg-muted text-muted-foreground',
  resolved: 'bg-success/10 text-success',
};

function ago(unixSecs: number): string {
  return relativeTime(new Date(unixSecs * 1000).toISOString()) ?? '';
}

/** One line of the timeline. */
export const EventItem = memo(function EventItem({
  event,
  showNode,
  relative,
}: {
  event: EventRow;
  showNode: boolean;
  /** "3h ago" rather than the clock time, for lists not grouped by day. */
  relative?: boolean;
}) {
  const tone = eventTone(event);
  const Icon = tone === 'resolved' ? CircleCheck : categoryOf(event.category).icon;
  const crash = event.kind === 'crashed' || event.kind === 'oom_killed';
  const keepsLogs = useNodeStore((s) => crash && !!s.meta[event.nodeId]?.capabilities.event_logs);
  return (
    <div data-event className='flex items-start gap-3 px-4 py-2.5'>
      <div className={cn('mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md', TONE[tone])}>
        <Icon className='size-3.5' />
      </div>
      <div className='min-w-0 flex-1'>
        <p className='text-sm wrap-anywhere'>{event.title}</p>
        {event.detail && <p className='text-xs text-muted-foreground'>{event.detail}</p>}
        {(event.actor || showNode) && (
          <p className='flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground'>
            {event.actor && <span className='truncate'>by {event.actor}</span>}
            {event.actor && showNode && <span aria-hidden='true'>on</span>}
            {showNode && <NodeName nodeId={event.nodeId} />}
          </p>
        )}
        {keepsLogs && <CrashLines nodeId={event.nodeId} eventId={event.id} />}
      </div>
      <time
        dateTime={new Date(event.at * 1000).toISOString()}
        title={new Date(event.at * 1000).toLocaleString()}
        className='shrink-0 text-xs tabular-nums text-muted-foreground'
      >
        {relative ? ago(event.at) : clockTime(event.at)}
      </time>
    </div>
  );
});

/** An open problem: what's wrong now, and since when. */
export const ProblemItem = memo(function ProblemItem({
  problem,
  showNode,
}: {
  problem: ProblemRow;
  showNode: boolean;
}) {
  const Icon = categoryOf(problem.category).icon;
  const acked = !!problem.acked_by;
  return (
    <div data-problem data-acked={acked || undefined} className='flex items-start gap-3 px-4 py-2.5'>
      <div
        className={cn(
          'mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md',
          acked ? 'bg-muted text-muted-foreground' : TONE[problem.severity],
        )}
      >
        <Icon className='size-3.5' />
      </div>
      <div className='min-w-0 flex-1'>
        <p className={cn('text-sm wrap-anywhere', acked && 'text-muted-foreground')}>{problem.title}</p>
        {problem.detail && <p className='text-xs text-muted-foreground'>{problem.detail}</p>}
        {problem.depends_on && (
          <p className='text-xs text-muted-foreground'>
            Goes through {problem.depends_on}: no alert of its own while {problem.depends_on} is unreachable.
          </p>
        )}
        {acked && (
          <p className='text-xs text-muted-foreground'>
            Acknowledged by {problem.acked_by}
            {problem.acked_at ? ` ${ago(problem.acked_at)}` : ''}. No more notifications until it clears.
          </p>
        )}
        {showNode && (
          <p className='flex min-w-0 text-xs'>
            <NodeName nodeId={problem.nodeId} />
          </p>
        )}
      </div>
      <span className='shrink-0 text-xs text-muted-foreground'>opened {ago(problem.opened_at)}</span>
      <ProblemMenu problem={problem} />
    </div>
  );
});
