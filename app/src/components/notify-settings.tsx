import { useState } from 'react';
import { BellOff, PencilLine, Plus, Send, Trash } from 'lucide-react';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@sunstead/ui/components/dialog';
import { Field, FieldDescription, FieldGroup, FieldLabel } from '@sunstead/ui/components/field';
import { Input } from '@sunstead/ui/components/input';
import { Button } from '@sunstead/ui/components/button';
import { Badge } from '@sunstead/ui/components/badge';
import { Checkbox } from '@sunstead/ui/components/checkbox';
import { useNotifyActions, useNotifyAll } from '@/api/queries';
import { nodeDisplayName, useNodeName, useNodeStore } from '@/stores/nodes';
import { DeviceLevel, useNotificationPrefs } from '@/stores/notifications';
import { CATEGORY, categoryOf } from '@/lib/events';
import { relativeTime } from '@/lib/time';
import { isDesktop } from '@/lib/platform';
import { ChannelKind } from '@/generated/ChannelKind';
import { EventCategory } from '@/generated/EventCategory';
import { NotifyChannel } from '@/generated/NotifyChannel';
import { NotifyChannelInput } from '@/generated/NotifyChannelInput';
import { Severity } from '@/generated/Severity';
import { Section } from './section';
import { SegmentedControl } from './segmented-control';
import { ConfirmDialog } from './confirm-dialog';
import { EmptyState } from './empty-state';
import { TableSkeleton } from './skeletons';
import { NodeName } from './node-name';

const CATEGORIES = Object.keys(CATEGORY) as EventCategory[];

function Row({ label, description, children }: { label: string; description: string; children: React.ReactNode }) {
  return (
    <div className='flex flex-wrap items-center justify-between gap-x-4 gap-y-2 p-4'>
      <div className='min-w-0'>
        <p className='text-sm'>{label}</p>
        <p className='text-xs text-muted-foreground'>{description}</p>
      </div>
      {children}
    </div>
  );
}

/** What this device itself notifies about. */
export function DeviceNotifications() {
  const level = useNotificationPrefs((s) => s.level);
  const setLevel = useNotificationPrefs((s) => s.setLevel);
  const after = useNotificationPrefs((s) => s.unreachableAfterMin);
  const setAfter = useNotificationPrefs((s) => s.setUnreachableAfter);
  const how = isDesktop() ? 'System notifications when the app is in the background.' : 'Toasts while this tab is open.';

  return (
    <Section title='Notifications on this device' contentClassName='divide-y'>
      <Row label='Events' description={how}>
        <SegmentedControl<DeviceLevel>
          label='Events on this device'
          value={level}
          onChange={setLevel}
          options={[
            { value: 'problems', label: 'Problems' },
            { value: 'errors', label: 'Errors' },
            { value: 'off', label: 'Off' },
          ]}
        />
      </Row>
      <Row label='Unreachable nodes' description='A node that stops answering for this long.'>
        <SegmentedControl<string>
          label='Unreachable after'
          value={String(after)}
          onChange={(v) => setAfter(Number(v))}
          options={[
            { value: '1', label: '1 min' },
            { value: '3', label: '3 min' },
            { value: '10', label: '10 min' },
            { value: '0', label: 'Off' },
          ]}
        />
      </Row>
    </Section>
  );
}

function rulesSummary(c: NotifyChannel): string {
  const what = { info: 'Everything', warning: 'Warnings and errors', error: 'Errors only' }[c.min_severity];
  const where = c.categories.length ? ` from ${c.categories.map((k) => categoryOf(k).label.toLowerCase()).join(', ')}` : '';
  return `${what}${where}${c.recoveries ? ', with recoveries' : ''}`;
}

function iso(unixSecs: number | null): string | null {
  return unixSecs === null ? null : new Date(unixSecs * 1000).toISOString();
}

function ChannelStatusLine({ channel }: { channel: NotifyChannel }) {
  const { last_ok_at, last_error_at, last_error } = channel.status;
  if (last_error_at !== null && (last_ok_at === null || last_error_at > last_ok_at)) {
    return (
      <p className='text-xs text-error'>
        Failed {relativeTime(iso(last_error_at))}: {last_error}
      </p>
    );
  }
  if (last_ok_at !== null) {
    return <p className='text-xs text-success'>Delivered {relativeTime(iso(last_ok_at))}</p>;
  }
  return null;
}

/** A node's link setting: what tapping one of its notifications opens. */
function LinkSetting({ nodeId, saved, canEdit, showNode }: { nodeId: string; saved: string; canEdit: boolean; showNode: boolean }) {
  const { saveSettings } = useNotifyActions();
  const name = useNodeName(nodeId);
  const [link, setLink] = useState<string | null>(null);
  return (
    <form
      className='p-4'
      onSubmit={(e) => {
        e.preventDefault();
        void saveSettings(nodeId, { link_url: (link ?? saved).trim() || null });
        setLink(null);
      }}
    >
      <Field>
        <FieldLabel htmlFor={`link-${nodeId}`}>{showNode ? `Link from ${name ?? nodeId}` : 'Link'}</FieldLabel>
        <div className='flex gap-2'>
          <Input
            id={`link-${nodeId}`}
            value={link ?? saved}
            placeholder='https://helion.example.com/events'
            disabled={!canEdit}
            onChange={(e) => setLink(e.target.value)}
          />
          {canEdit && (
            <Button type='submit' variant='outline' disabled={link === null || link === saved}>
              Save
            </Button>
          )}
        </div>
        <FieldDescription>Opened when you tap a notification.</FieldDescription>
      </Field>
    </form>
  );
}

type Editing = { nodeId: string; channel: NotifyChannel } | 'new' | null;

/**
 * Every node's channels in one list. Each node sends its own notifications,
 * so a channel belongs to a node; a new one can be added to several at once.
 * Admins only; editing also needs `allow_actions`.
 */
export function NotificationChannels({ nodeIds }: { nodeIds: string[] }) {
  const meta = useNodeStore((s) => s.meta);
  const { byNode, errors, loading } = useNotifyAll(nodeIds);
  const { remove, test, pending } = useNotifyActions();
  const [editing, setEditing] = useState<Editing>(null);
  const showNode = nodeIds.length > 1;
  const editable = nodeIds.filter((id) => meta[id]?.capabilities.notify_actions);
  const rows = nodeIds.flatMap((nodeId) => (byNode[nodeId]?.channels ?? []).map((channel) => ({ nodeId, channel })));

  return (
    <Section
      title='Notification channels'
      count={rows.length || undefined}
      actions={
        editable.length > 0 && (
          <Button variant='outline' size='sm' onClick={() => setEditing('new')}>
            <Plus /> Add channel
          </Button>
        )
      }
      contentClassName='divide-y'
    >
      {loading && rows.length === 0 ? (
        <TableSkeleton columns={2} rows={2} />
      ) : (
        <>
          {errors.map((e) => (
            <p key={e.nodeId} className='p-4 text-sm text-error'>
              {showNode ? <NodeName nodeId={e.nodeId} /> : null} {e.message}
            </p>
          ))}
          {rows.length === 0 && errors.length === 0 && (
            <EmptyState
              size='inline'
              icon={BellOff}
              title='No channels'
              description='Add an ntfy topic or a webhook to hear about problems away from this app.'
            />
          )}
          {rows.map(({ nodeId, channel: c }) => {
            const canEdit = editable.includes(nodeId);
            return (
              <div key={`${nodeId}:${c.id}`} data-channel className='flex flex-wrap items-center gap-3 px-4 py-3'>
                <div className='min-w-0 flex-1'>
                  <div className='flex items-center gap-2'>
                    <p className='truncate font-medium'>{c.name}</p>
                    <Badge variant='outline' className='text-2xs'>
                      {c.kind === 'ntfy' ? 'ntfy' : 'Webhook'}
                    </Badge>
                    {!c.enabled && (
                      <Badge variant='secondary' className='text-2xs'>
                        Off
                      </Badge>
                    )}
                    {showNode && (
                      <span className='flex min-w-0 gap-1 text-xs text-muted-foreground'>
                        from <NodeName nodeId={nodeId} />
                      </span>
                    )}
                  </div>
                  <p className='selectable truncate font-mono text-2xs text-muted-foreground'>
                    {c.kind === 'ntfy' ? `${c.url.replace(/\/$/, '')}/${c.topic}` : c.url}
                  </p>
                  <p className='text-xs text-muted-foreground'>{rulesSummary(c)}</p>
                  <ChannelStatusLine channel={c} />
                </div>
                {canEdit && (
                  <div className='flex items-center gap-1'>
                    <Button
                      variant='outline'
                      size='sm'
                      disabled={pending === `test:${nodeId}:${c.id}`}
                      onClick={() => void test(nodeId, c.id, c.name)}
                    >
                      <Send /> Send test
                    </Button>
                    <Button
                      variant='ghost'
                      size='icon'
                      aria-label={`Edit ${c.name}`}
                      onClick={() => setEditing({ nodeId, channel: c })}
                    >
                      <PencilLine />
                    </Button>
                    <ConfirmDialog
                      trigger={
                        <Button variant='ghost' size='icon' aria-label={`Remove ${c.name}`}>
                          <Trash />
                        </Button>
                      }
                      title={`Remove ${c.name}?`}
                      description='It stops getting notifications. Its saved token is deleted.'
                      confirmLabel='Remove'
                      destructive
                      onConfirm={() => void remove(nodeId, c.id, c.name)}
                    />
                  </div>
                )}
              </div>
            );
          })}
          {nodeIds
            .filter((id) => byNode[id])
            .map((id) => (
              <LinkSetting
                key={id}
                nodeId={id}
                saved={byNode[id].settings.link_url ?? ''}
                canEdit={editable.includes(id)}
                showNode={showNode}
              />
            ))}
        </>
      )}
      <Dialog open={editing !== null} onOpenChange={(o) => !o && setEditing(null)}>
        {editing !== null && (
          <ChannelForm
            nodeIds={editing === 'new' ? editable : [editing.nodeId]}
            channel={editing === 'new' ? undefined : editing.channel}
            onDone={() => setEditing(null)}
          />
        )}
      </Dialog>
    </Section>
  );
}

function ChannelForm({
  nodeIds,
  channel,
  onDone,
}: {
  /** Where it can go: the channel's own node when editing. */
  nodeIds: string[];
  channel?: NotifyChannel;
  onDone: () => void;
}) {
  const { save } = useNotifyActions();
  const allNodes = useNodeStore((s) => s.nodes);
  // A new channel with several nodes to send from: tick the ones it's for
  // (each node's topic and token usually differ, so none by default).
  const [targets, setTargets] = useState<string[]>(nodeIds.length === 1 ? nodeIds : []);
  const [name, setName] = useState(channel?.name ?? '');
  const [kind, setKind] = useState<ChannelKind>(channel?.kind ?? 'ntfy');
  const [url, setUrl] = useState(channel?.url ?? '');
  const [topic, setTopic] = useState(channel?.topic ?? '');
  // Blank keeps the saved secret; `clearSecret` removes it.
  const [secret, setSecret] = useState('');
  const [clearSecret, setClearSecret] = useState(false);
  const [enabled, setEnabled] = useState(channel?.enabled ?? true);
  const [minSeverity, setMinSeverity] = useState<Severity>(channel?.min_severity ?? 'warning');
  const [recoveries, setRecoveries] = useState(channel?.recoveries ?? true);
  const [categories, setCategories] = useState<EventCategory[]>(channel?.categories ?? []);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const toggleCategory = (c: EventCategory, on: boolean) =>
    setCategories((all) => (on ? [...all, c] : all.filter((x) => x !== c)));

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const input: NotifyChannelInput = {
      name,
      kind,
      url,
      topic: kind === 'ntfy' ? topic : undefined,
      enabled,
      min_severity: minSeverity,
      recoveries,
      categories,
    };
    if (secret) input.secret = secret;
    else if (clearSecret) input.secret = '';

    setBusy(true);
    setError(null);
    try {
      // One at a time, so a failure says where and stops before the rest.
      for (const nodeId of targets) await save(nodeId, input, channel?.id);
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not save.');
    } finally {
      setBusy(false);
    }
  };

  const secretLabel = kind === 'ntfy' ? 'Access token' : 'Bearer token';
  return (
    // The tallest form in the app; it scrolls rather than run off a short window.
    <DialogContent className='max-h-[calc(100dvh-2rem)] overflow-y-auto'>
      <DialogHeader>
        <DialogTitle>
          {channel ? `Edit ${channel.name}` : 'Add a notification channel'}
        </DialogTitle>
        <DialogDescription>
          The node sends these itself, so they arrive even when this app is closed.
        </DialogDescription>
      </DialogHeader>

      <form id='notify-channel' onSubmit={(e) => void submit(e)}>
        <FieldGroup>
          {!channel && nodeIds.length > 1 && (
            <Field>
              <FieldLabel>Send from</FieldLabel>
              <div className='flex flex-wrap gap-x-4 gap-y-2'>
                {nodeIds.map((id) => {
                  const node = allNodes.find((n) => n.id === id);
                  return (
                    <label key={id} className='flex items-center gap-2 text-sm'>
                      <Checkbox
                        checked={targets.includes(id)}
                        onCheckedChange={(v) =>
                          setTargets((all) => (v === true ? [...all, id] : all.filter((x) => x !== id)))
                        }
                      />
                      {node ? nodeDisplayName(node) : id}
                    </label>
                  );
                })}
              </div>
              <FieldDescription>Each node sends its own events. The same settings go to each one ticked.</FieldDescription>
            </Field>
          )}
          <Field>
            <FieldLabel htmlFor='channel-name'>Name</FieldLabel>
            <Input
              id='channel-name'
              value={name}
              placeholder='phone'
              onChange={(e) => setName(e.target.value)}
              autoFocus
              required
            />
          </Field>

          <Field>
            <FieldLabel>Type</FieldLabel>
            <SegmentedControl<ChannelKind>
              label='Type'
              value={kind}
              onChange={setKind}
              options={[
                { value: 'ntfy', label: 'ntfy' },
                { value: 'webhook', label: 'Webhook' },
              ]}
            />
          </Field>

          <Field>
            <FieldLabel htmlFor='channel-url'>
              {kind === 'ntfy' ? 'Server' : 'URL'}
            </FieldLabel>
            <Input
              id='channel-url'
              value={url}
              placeholder={
                kind === 'ntfy' ? 'https://ntfy.example.com' : 'https://example.com/hook'
              }
              onChange={(e) => setUrl(e.target.value)}
              className='font-mono'
              required
            />
            {kind === 'webhook' && (
              <FieldDescription>Each event is POSTed to it as JSON.</FieldDescription>
            )}
          </Field>

          {kind === 'ntfy' && (
            <Field>
              <FieldLabel htmlFor='channel-topic'>Topic</FieldLabel>
              <Input
                id='channel-topic'
                value={topic}
                placeholder='helion-jupiter'
                onChange={(e) => setTopic(e.target.value)}
                className='font-mono'
                required
              />
              <FieldDescription>
                Subscribe to the same topic in the ntfy app.
              </FieldDescription>
            </Field>
          )}

          <Field>
            <FieldLabel htmlFor='channel-secret'>{secretLabel}</FieldLabel>
            <Input
              id='channel-secret'
              type='password'
              autoComplete='off'
              value={secret}
              placeholder={
                channel?.has_secret && !clearSecret
                  ? 'Saved. Type to replace it.'
                  : 'Optional'
              }
              onChange={(e) => setSecret(e.target.value)}
              className='font-mono'
            />
            {channel?.has_secret && !secret && (
              <label className='flex items-center gap-2 text-xs text-muted-foreground'>
                <Checkbox
                  checked={clearSecret}
                  onCheckedChange={(v) => setClearSecret(v === true)}
                />
                Remove the saved token
              </label>
            )}
          </Field>

          <Field>
            <FieldLabel>Send</FieldLabel>
            <SegmentedControl<Severity>
              label='Send'
              value={minSeverity}
              onChange={setMinSeverity}
              options={[
                { value: 'warning', label: 'Problems' },
                { value: 'error', label: 'Errors' },
                { value: 'info', label: 'Everything' },
              ]}
            />
            <label className='flex items-center gap-2 text-sm'>
              <Checkbox
                checked={recoveries}
                onCheckedChange={(v) => setRecoveries(v === true)}
              />
              Also say when a problem clears
            </label>
          </Field>

          <Field>
            <FieldLabel>From</FieldLabel>
            <div className='grid grid-cols-2 gap-2'>
              {CATEGORIES.map((c) => (
                <label key={c} className='flex items-center gap-2 text-sm'>
                  <Checkbox
                    checked={categories.includes(c)}
                    onCheckedChange={(v) => toggleCategory(c, v === true)}
                  />
                  {categoryOf(c).label}
                </label>
              ))}
            </div>
            <FieldDescription>None ticked means all of them.</FieldDescription>
          </Field>

          <label className='flex items-center gap-2 text-sm'>
            <Checkbox checked={enabled} onCheckedChange={(v) => setEnabled(v === true)} />
            On
          </label>

          {error && <p className='text-sm text-error'>{error}</p>}
        </FieldGroup>
      </form>

      <DialogFooter>
        <DialogClose render={<Button variant='outline' />}>Cancel</DialogClose>
        <Button type='submit' form='notify-channel' disabled={busy || targets.length === 0}>
          {channel ? 'Save' : 'Add'}
        </Button>
      </DialogFooter>
    </DialogContent>
  );
}
