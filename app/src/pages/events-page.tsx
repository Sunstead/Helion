import { Fragment, useEffect, useMemo, useState } from 'react';
import { CircleCheck, History, SearchX } from 'lucide-react';
import { toast } from 'sonner';
import { useTick } from '@/api/queries';
import { useEventsStore } from '@/stores/events';
import { getConnection, useNodeStore } from '@/stores/nodes';
import { useNotificationPrefs } from '@/stores/notifications';
import { useAwaiting } from '@/hooks/use-awaiting';
import { dayLabel, EventRow, mergeEvents, mergeProblems } from '@/lib/events';
import { matchesQuery } from '@/lib/format';
import { PageHeader } from '@/components/page-header';
import { SearchInput } from '@/components/search-input';
import { SegmentedControl } from '@/components/segmented-control';
import { Section } from '@/components/section';
import { EmptyState, NoNodesState } from '@/components/empty-state';
import { TableSkeleton } from '@/components/skeletons';
import { SETUP } from '@/lib/setup';
import { EventItem, ProblemItem } from '@/components/event-item';
import { ActiveSilences } from '@/components/active-silences';
import { Button } from '@sunstead/ui/components/button';
import { NodeSelect } from '@/components/node-select';
import { ALL_NODES, useNodeScope } from '@/hooks/use-node-scope';

type Filter = 'all' | 'problems' | 'actions';

const OLDER_PAGE = 200;

function matchesFilter(e: EventRow, filter: Filter): boolean {
  if (filter === 'problems') return e.severity !== 'info' || e.problem !== null;
  if (filter === 'actions') return e.category === 'action';
  return true;
}

/** Days, newest first, each with its events. */
function byDay(events: EventRow[]): [string, EventRow[]][] {
  const days: [string, EventRow[]][] = [];
  for (const e of events) {
    const label = dayLabel(e.at);
    const last = days.at(-1);
    if (last && last[0] === label) last[1].push(e);
    else days.push([label, [e]]);
  }
  return days;
}

export function EventsPage() {
  const nodeCount = useNodeStore((s) => s.nodes.length);
  const anySupported = useNodeStore((s) =>
    s.nodes.some((n) => s.meta[n.id]?.status === 'online' && s.meta[n.id]?.capabilities.events),
  );
  const byNode = useEventsStore((s) => s.byNode);
  const awaiting = useAwaiting(byNode);
  const markSeen = useNotificationPrefs((s) => s.markSeen);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [loadingOlder, setLoadingOlder] = useState(false);
  useTick(60_000);

  const [scope, setScope, scopeNodes] = useNodeScope('events', {
    capability: 'events',
    allowAll: true,
    initial: ALL_NODES,
  });
  const inScope = (nodeId: string) => scope === ALL_NODES || nodeId === scope;

  const all = useMemo(() => mergeEvents(byNode), [byNode]);
  const problems = useMemo(
    () => mergeProblems(byNode).filter((p) => scope === ALL_NODES || p.nodeId === scope),
    [byNode, scope],
  );
  const visible = useMemo(
    () =>
      all.filter(
        (e) =>
          (scope === ALL_NODES || e.nodeId === scope) &&
          matchesFilter(e, filter) &&
          matchesQuery(query, e.title, e.detail, e.subject, e.actor, e.service),
      ),
    [all, filter, query, scope],
  );
  const days = useMemo(() => byDay(visible), [visible]);
  const showNode = nodeCount > 1 && scope === ALL_NODES;
  const hasOlder = Object.entries(byNode).some(([id, n]) => inScope(id) && n.hasOlder);
  const filtered = query !== '' || filter !== 'all' || scope !== ALL_NODES;

  // Whatever is on screen has been seen, for the bell.
  useEffect(() => {
    for (const [nodeId, n] of Object.entries(byNode)) {
      const newest = n.events[0];
      if (newest) markSeen(nodeId, newest.id);
    }
  }, [byNode, markSeen]);

  async function loadOlder() {
    setLoadingOlder(true);
    try {
      await Promise.all(
        Object.entries(useEventsStore.getState().byNode)
          .filter(([id, n]) => inScope(id) && n.hasOlder)
          .map(async ([nodeId, n]) => {
            const conn = getConnection(nodeId);
            const oldest = n.events.at(-1);
            if (!conn || !oldest) return;
            const page = await conn.client.getEvents({ before: oldest.id, limit: OLDER_PAGE });
            useEventsStore.getState().addOlder(nodeId, page.events, page.more);
          }),
      );
    } catch (e) {
      toast.error('Could not load older events', { description: e instanceof Error ? e.message : undefined });
    } finally {
      setLoadingOlder(false);
    }
  }

  function body() {
    if (nodeCount === 0) return <NoNodesState />;
    if (!anySupported && !awaiting && all.length === 0) {
      return (
        <EmptyState
          size='page'
          icon={History}
          title='No event log'
          description='None of your nodes keep one yet.'
          setup={SETUP.events}
        />
      );
    }
    return (
      <>
        <ActiveSilences />
        <Section title='Open problems' count={problems.length || undefined} contentClassName='divide-y'>
          {awaiting ? (
            <TableSkeleton columns={2} rows={2} />
          ) : problems.length === 0 ? (
            <div className='flex items-center gap-2 px-4 py-3 text-sm text-muted-foreground'>
              <CircleCheck className='size-4 text-success' />
              Nothing needs attention.
            </div>
          ) : (
            problems.map((p) => <ProblemItem key={`${p.nodeId}:${p.key}`} problem={p} showNode={showNode} />)
          )}
        </Section>

        <Section title='Timeline' contentClassName='divide-y'>
          {awaiting ? (
            <TableSkeleton columns={3} rows={6} />
          ) : visible.length === 0 ? (
            filtered ? (
              <EmptyState size='inline' icon={SearchX} title='No matching events' />
            ) : (
              <EmptyState size='card' icon={History} title='No events yet' />
            )
          ) : (
            days.map(([label, events]) => (
              <Fragment key={label}>
                <h3 className='label-hud bg-muted/40 px-4 py-1.5 text-2xs text-muted-foreground'>{label}</h3>
                {events.map((e) => (
                  <EventItem key={`${e.nodeId}:${e.id}`} event={e} showNode={showNode} />
                ))}
              </Fragment>
            ))
          )}
          {hasOlder && !awaiting && (
            <div className='flex justify-center p-3'>
              <Button variant='outline' size='sm' onClick={() => void loadOlder()} disabled={loadingOlder}>
                {loadingOlder ? 'Loading' : 'Load older'}
              </Button>
            </div>
          )}
        </Section>
      </>
    );
  }

  return (
    <>
      <PageHeader
        title='Events'
        count={all.length ? visible.length : undefined}
        actions={
          all.length > 0 && (
            <>
              <NodeSelect value={scope} onChange={setScope} candidates={scopeNodes} allowAll />
              <SearchInput value={query} onChange={setQuery} placeholder='Search events' />
              <SegmentedControl
                label='Show'
                value={filter}
                onChange={setFilter}
                options={[
                  { value: 'all', label: 'All' },
                  { value: 'problems', label: 'Problems' },
                  { value: 'actions', label: 'Actions' },
                ]}
              />
            </>
          )
        }
      />
      {body()}
    </>
  );
}
