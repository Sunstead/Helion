import { useMemo, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import { useEventLogs } from '@/api/queries';
import { AnsiText } from './ansi-text';
import { formatLogTime } from '@/lib/log-line';
import { cn } from '@/lib/utils';

/** A crash's last lines, kept with its event, opened on request. */
export function CrashLines({ nodeId, eventId }: { nodeId: string; eventId: number }) {
  const [open, setOpen] = useState(false);
  const { data, isLoading, error } = useEventLogs(nodeId, eventId, open);
  const lines = useMemo(() => data?.lines ?? [], [data]);

  return (
    <div className='mt-1'>
      <button
        type='button'
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className='flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground'
      >
        <ChevronRight className={cn('size-3 transition-transform', open && 'rotate-90')} />
        Last lines before it stopped
      </button>
      {open && (
        <div className='mt-1.5 max-h-72 overflow-auto rounded-md border bg-muted/40 p-2 font-mono text-2xs leading-relaxed'>
          {isLoading ? (
            <p className='text-muted-foreground'>Loading</p>
          ) : error ? (
            <p className='text-muted-foreground'>Could not load them.</p>
          ) : lines.length === 0 ? (
            <p className='text-muted-foreground'>No lines were kept.</p>
          ) : (
            lines.map((l, i) => (
              <div key={i} className='flex gap-2 whitespace-pre-wrap wrap-anywhere'>
                {l.ts && <span className='shrink-0 text-muted-foreground'>{formatLogTime(l.ts)}</span>}
                <AnsiText text={l.text} />
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
