import { useState } from 'react';
import { BellOff, Check, MoreHorizontal, Undo2, X } from 'lucide-react';
import { useAlertActions, useForgetService, useNodeMeta } from '@/api/queries';
import { ProblemRow } from '@/lib/events';
import { getConnection } from '@/stores/nodes';
import { Button } from '@sunstead/ui/components/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@sunstead/ui/components/dropdown-menu';
import { ConfirmDialog } from './confirm-dialog';

/** How long a silence lasts; `null` until someone ends it. */
const FOR: { label: string; minutes: number | null }[] = [
  { label: '1 hour', minutes: 60 },
  { label: '4 hours', minutes: 240 },
  { label: '24 hours', minutes: 1_440 },
  { label: 'Until I end it', minutes: null },
];

function SilenceFor({ label, onPick }: { label: string; onPick: (minutes: number | null) => void }) {
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <BellOff /> {label}
      </DropdownMenuSubTrigger>
      <DropdownMenuPortal>
        <DropdownMenuSubContent>
          {FOR.map((f) => (
            <DropdownMenuItem key={f.label} onClick={() => onPick(f.minutes)}>
              {f.label}
            </DropdownMenuItem>
          ))}
        </DropdownMenuSubContent>
      </DropdownMenuPortal>
    </DropdownMenuSub>
  );
}

/**
 * What an admin can do about an open problem: acknowledge it, silence its
 * service or node for a while, or forget a service that's gone for good.
 * Renders nothing for a viewer or an agent without these.
 */
export function ProblemMenu({ problem }: { problem: ProblemRow }) {
  const meta = useNodeMeta(problem.nodeId);
  const { ack, unack, silence } = useAlertActions();
  const forget = useForgetService();
  const [forgetting, setForgetting] = useState(false);

  if (!meta?.capabilities.alert_actions) return null;
  const missing = problem.category === 'service' && problem.kind === 'missing';
  const canForget = missing && meta.capabilities.expected_services && meta.capabilities.container_actions;
  const service = problem.service;
  const swallow = () => {};

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<Button variant='ghost' size='icon' className='-my-1 size-7' aria-label={`Actions for ${problem.title}`} />}
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align='end' className='w-56'>
          {problem.acked_by ? (
            <DropdownMenuItem onClick={() => void unack(problem.nodeId, problem.key).catch(swallow)}>
              <Undo2 /> Unacknowledge
            </DropdownMenuItem>
          ) : (
            <DropdownMenuItem onClick={() => void ack(problem.nodeId, problem.key).catch(swallow)}>
              <Check /> Acknowledge
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuLabel>Hold notifications</DropdownMenuLabel>
            {service && (
              <SilenceFor
                label={`Silence ${service}`}
                onPick={(minutes) =>
                  void silence(problem.nodeId, { scope: 'service', target: service, minutes, reason: null }, service).catch(
                    swallow,
                  )
                }
              />
            )}
            <SilenceFor
              label='Silence this node'
              onPick={(minutes) =>
                void silence(problem.nodeId, { scope: 'node', target: null, minutes, reason: null }, 'this node').catch(
                  swallow,
                )
              }
            />
          </DropdownMenuGroup>
          {canForget && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setForgetting(true)}>
                <X /> Forget {problem.subject}
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {canForget && (
        <ConfirmDialog
          open={forgetting}
          onOpenChange={setForgetting}
          title={`Forget ${problem.subject}?`}
          description='It will no longer be expected to run, and this problem closes. If it runs for 10 minutes again, it is expected again.'
          confirmLabel='Forget'
          destructive={false}
          onConfirm={async () => {
            await forget(problem.nodeId, problem.subject);
            await getConnection(problem.nodeId)?.refreshEvents();
          }}
        />
      )}
    </>
  );
}
