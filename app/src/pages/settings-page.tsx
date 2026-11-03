import { useState } from 'react';
import { Info, PencilLine, RefreshCw, Trash } from 'lucide-react';
import { useNodeStore, nodeDisplayName } from '@/stores/nodes';
import { useNodeMeta } from '@/api/queries';
import { PageHeader } from '@/components/page-header';
import { Section } from '@/components/section';
import { NodeStatusBadge } from '@/components/node-status-badge';
import { NodeAvatar } from '@/components/node-planet';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { AccountList, SignInButton } from '@/components/sign-in';
import { tokenStorageNote, useNodeSignIn } from '@/lib/sign-in';
import { RenameNodeDialog } from '@/components/rename-node-dialog';
import { AddNodeButton, EmptyState } from '@/components/empty-state';
import { Hint, ShortcutKeys } from '@/components/hint';
import { Button } from '@sunstead/ui/components/button';
import { Badge } from '@sunstead/ui/components/badge';
import { Popover, PopoverContent, PopoverTrigger } from '@sunstead/ui/components/popover';
import { PAGES } from '@/lib/navigation';
import { ShortcutId } from '@/lib/shortcuts';
import { getPlatform, isDesktop } from '@/lib/platform';
import { AppUpdateSettings } from '@/components/app-update-settings';
import { ServerOff } from 'lucide-react';
import { DeviceNotifications, NotificationChannels } from '@/components/notify-settings';
import { ActiveSilences } from '@/components/active-silences';
import { ThemeSettings } from '@/components/theme-picker';

const CAPS = [
  ['container_actions', 'Actions'],
  ['container_logs', 'Logs'],
  ['metrics_history', 'History'],
  ['backups', 'Backups'],
  ['tailnet', 'Tailnet'],
  ['uptime', 'Uptime'],
  ['backup_actions', 'Backup now'],
  ['updates', 'Updates'],
] as const;

function NodeRow({ nodeId }: { nodeId: string }) {
  const node = useNodeStore((s) => s.nodes.find((n) => n.id === nodeId));
  const removeNode = useNodeStore((s) => s.removeNode);
  const reconnect = useNodeStore((s) => s.reconnect);
  const meta = useNodeMeta(nodeId);
  const needsSignIn = useNodeSignIn(nodeId);
  const [dialog, setDialog] = useState<'rename' | null>(null);

  if (!node) return null;
  const name = nodeDisplayName(node);

  return (
    <div className='flex items-center gap-3 px-4 py-3'>
      <NodeAvatar nodeId={nodeId} size={32} />
      <div className='min-w-0 flex-1'>
        <div className='flex items-center gap-2'>
          <p className='truncate font-medium'>{name}</p>
          <NodeStatusBadge nodeId={nodeId} />
        </div>
        <div className='flex flex-wrap items-center gap-1.5 pt-0.5'>
          <span className='selectable font-mono text-2xs text-muted-foreground'>{node.url}</span>
          {meta?.agentVersion && (
            <Badge variant='outline' className='font-mono text-2xs'>
              v{meta.agentVersion}
            </Badge>
          )}
          {meta?.status === 'online' &&
            CAPS.filter(([key]) => meta.capabilities[key]).map(([key, label]) => (
              <Badge key={key} variant='secondary' className='text-2xs'>
                {label}
              </Badge>
            ))}
        </div>
      </div>

      <div className='flex items-center gap-1'>
        <Hint label='Rename'>
          <Button variant='ghost' size='icon' aria-label='Rename' onClick={() => setDialog('rename')}>
            <PencilLine />
          </Button>
        </Hint>
        {needsSignIn && <SignInButton auth={needsSignIn} />}
        <Hint label='Reconnect'>
          <Button variant='ghost' size='icon' aria-label='Reconnect' onClick={() => reconnect(nodeId)}>
            <RefreshCw />
          </Button>
        </Hint>
        <ConfirmDialog
          trigger={
            <Button variant='ghost' size='icon' aria-label='Remove'>
              <Trash />
            </Button>
          }
          title={`Remove ${name}?`}
          description='Helion forgets this node. The node itself is unchanged.'
          confirmLabel='Remove'
          onConfirm={() => removeNode(nodeId)}
        />
      </div>

      <RenameNodeDialog
        nodeId={nodeId}
        open={dialog === 'rename'}
        onOpenChange={(o) => setDialog(o ? 'rename' : null)}
      />
    </div>
  );
}

function SignInStorageInfo() {
  return (
    <Popover>
      <PopoverTrigger
        render={
          <Button variant='ghost' size='icon-xs' aria-label='About sign-in storage' />
        }
      >
        <Info />
      </PopoverTrigger>
      <PopoverContent className='w-72 text-sm'>{tokenStorageNote()}</PopoverContent>
    </Popover>
  );
}

const SHORTCUT_ROWS: { id: ShortcutId; label: string }[] = [
  { id: 'palette', label: 'Command palette' },
  { id: 'search', label: 'Search this page' },
  { id: 'sidebar', label: 'Toggle sidebar' },
  { id: 'settings', label: 'Settings' },
  ...PAGES.filter((p) => p.shortcut.startsWith('go.')).map((p) => ({
    id: p.shortcut,
    label: `Go to ${p.label}`,
  })),
];

const PLATFORM_LABEL = { macos: 'macOS', windows: 'Windows', linux: 'Linux', web: 'Browser' };

export function SettingsPage() {
  const nodes = useNodeStore((s) => s.nodes);
  // Joined so the list only changes when a node gains or loses the feature.
  const notifyKey = useNodeStore((s) =>
    s.nodes
      .filter((n) => s.meta[n.id]?.capabilities.notify)
      .map((n) => n.id)
      .join('|'),
  );
  const notifyNodes = notifyKey ? notifyKey.split('|') : [];

  return (
    <>
      <PageHeader title='Settings' />

      <Section
        title='Nodes'
        count={nodes.length || undefined}
        actions={<AddNodeButton variant='outline' />}
        contentClassName='divide-y'
      >
        {nodes.length === 0 ? (
          <EmptyState size='inline' icon={ServerOff} title='No nodes yet' />
        ) : (
          nodes.map((n) => <NodeRow key={n.id} nodeId={n.id} />)
        )}
      </Section>

      <Section
        title={
          <>
            Account
            <SignInStorageInfo />
          </>
        }
        contentClassName='divide-y'
      >
        <AccountList />
      </Section>

      <DeviceNotifications />
      <ActiveSilences />
      {notifyNodes.length > 0 && <NotificationChannels nodeIds={notifyNodes} />}

      <Section title='Appearance' data-appearance>
        <ThemeSettings />
      </Section>

      {isDesktop() ? (
        // The desktop app updates itself; the web UI is its agent's version.
        <Section title='About' contentClassName='divide-y'>
          <AppUpdateSettings platform={PLATFORM_LABEL[getPlatform()]} />
        </Section>
      ) : (
        <Section title='About' contentClassName='grid gap-2 p-4 text-sm @4xl:grid-cols-2 @4xl:gap-8'>
          <div className='flex justify-between'>
            <span className='text-muted-foreground'>Version</span>
            <span className='font-mono tabular-nums'>{__APP_VERSION__}</span>
          </div>
          <div className='flex justify-between'>
            <span className='text-muted-foreground'>Platform</span>
            <span>{PLATFORM_LABEL[getPlatform()]}</span>
          </div>
        </Section>
      )}

      <Section title='Keyboard shortcuts' contentClassName='grid @2xl:grid-cols-2'>
        {SHORTCUT_ROWS.map((r) => (
          <div key={r.id} className='flex items-center justify-between border-b px-4 py-2 text-sm'>
            <span className='text-muted-foreground'>{r.label}</span>
            <ShortcutKeys id={r.id} />
          </div>
        ))}
      </Section>
    </>
  );
}
