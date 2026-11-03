import { memo, useState } from 'react';
import { Link } from '@tanstack/react-router';
import { ChartLine, Copy, Logs, MoreHorizontal, Play, RotateCcw, Square, Trash } from 'lucide-react';
import { ContainerRow } from './container-columns';
import { useContainerActions, useNodeMeta } from '@/api/queries';
import { copyText } from '@/lib/clipboard';
import { Button } from '@sunstead/ui/components/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@sunstead/ui/components/dropdown-menu';
import { ConfirmDialog } from './confirm-dialog';
import { ReadOnlyNote } from './read-only-note';
import { ContainerHistorySheet } from './container-history-sheet';

export const ContainerActionsCell = memo(function ContainerActionsCell({
  container,
}: {
  container: ContainerRow;
}) {
  const meta = useNodeMeta(container.nodeId);
  const { run, pending } = useContainerActions(container.nodeId);
  const [removing, setRemoving] = useState(false);
  const [history, setHistory] = useState(false);

  const canAct = meta?.capabilities.container_actions ?? false;
  const running = container.state === 'running';
  const busy = !!pending;

  return (
    <div className='flex justify-end'>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant='ghost'
              size='icon'
              aria-label={`Actions for ${container.name}`}
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align='end' className='w-52'>
          <DropdownMenuGroup>
            <DropdownMenuLabel className='truncate'>{container.name}</DropdownMenuLabel>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            render={
              <Link
                to='/logs'
                search={{ node: container.nodeId, container: container.id }}
              />
            }
          >
            <Logs /> View logs
          </DropdownMenuItem>
          {meta?.capabilities.container_history && (
            <DropdownMenuItem onClick={() => setHistory(true)}>
              <ChartLine /> View history
            </DropdownMenuItem>
          )}
          <DropdownMenuItem
            onClick={() => void copyText(container.id, 'Container ID copied')}
          >
            <Copy /> Copy ID
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            disabled={!canAct || busy || running}
            onClick={() => void run(container.id, 'start', container.name)}
          >
            <Play /> Start
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!canAct || busy || !running}
            onClick={() => void run(container.id, 'restart', container.name)}
          >
            <RotateCcw /> Restart
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!canAct || busy || !running}
            onClick={() => void run(container.id, 'stop', container.name)}
          >
            <Square /> Stop
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            variant='destructive'
            disabled={!canAct || busy}
            onClick={() => setRemoving(true)}
          >
            <Trash /> Remove
          </DropdownMenuItem>
          {!canAct && <ReadOnlyNote />}
        </DropdownMenuContent>
      </DropdownMenu>

      {history && (
        <ContainerHistorySheet
          nodeId={container.nodeId}
          name={container.name}
          open={history}
          onOpenChange={setHistory}
        />
      )}

      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={`Remove ${container.name}?`}
        description={
          running
            ? 'The container will be stopped and removed. Named volumes are kept.'
            : 'Named volumes are kept.'
        }
        confirmLabel='Remove'
        onConfirm={async () => {
          await run(container.id, 'remove', container.name, { force: running });
        }}
      />
    </div>
  );
});
