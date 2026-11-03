import {
  Archive,
  CalendarClock,
  CircleCheck,
  CircleX,
  HardDrive,
  History,
  Layers,
  LoaderCircle,
  RotateCcw,
  TriangleAlert,
} from 'lucide-react';
import { useState } from 'react';
import { useNodeStore } from '@/stores/nodes';
import { useBackups, useNodeMeta, useTick } from '@/api/queries';
import { BackupsStatus } from '@/generated/BackupsStatus';
import { StepStatus } from '@/generated/StepStatus';
import { formatBytes } from '@/lib/node-metrics';
import { relativeTime, secondsToDuration } from '@/lib/time';
import { NO_VALUE, sentence } from '@/lib/format';
import { backupHealth } from '@/lib/backups';
import { PageHeader } from '@/components/page-header';
import { NodeSelect } from '@/components/node-select';
import { useNodeScope } from '@/hooks/use-node-scope';
import { StatCard, StatRow } from '@/components/stat-card';
import { Section } from '@/components/section';
import { EmptyState, NoNodesState } from '@/components/empty-state';
import { NodeUnavailable } from '@/components/sign-in';
import { SETUP } from '@/lib/setup';
import { SetupHint } from '@/components/setup-hint';
import { Alert, AlertDescription, AlertTitle } from '@sunstead/ui/components/alert';
import { Badge } from '@sunstead/ui/components/badge';
import { Skeleton } from '@sunstead/ui/components/skeleton';
import { Button } from '@sunstead/ui/components/button';
import {
  BackUpNowButton,
  RequestsSection,
  RestoreTestSection,
} from '@/components/backup-controls';
import { RestoreGuide } from '@/components/restore-guide';
import { BackupSnapshotInfo } from '@/generated/BackupSnapshotInfo';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@sunstead/ui/components/table';

function HealthAlert({ status }: { status: BackupsStatus }) {
  const health = backupHealth(status);
  const lastRun = relativeTime(status.last_run);
  const next = relativeTime(status.next_run);

  if (health === 'running') {
    return (
      <Alert>
        <LoaderCircle className='animate-spin text-muted-foreground' />
        <AlertTitle>Backup running</AlertTitle>
        <AlertDescription>
          {sentence([`started ${relativeTime(status.timer_last_fired)}`, lastRun && `last finished ${lastRun}`])}
        </AlertDescription>
      </Alert>
    );
  }

  if (health === 'healthy') {
    return (
      <Alert className='border-success/30'>
        <CircleCheck className='text-success' />
        <AlertTitle>Backups healthy</AlertTitle>
        <AlertDescription>
          {sentence([
            lastRun && `last run ${lastRun}`,
            status.duration_secs != null && `took ${secondsToDuration(status.duration_secs)}`,
            next && `next ${next}`,
          ])}
        </AlertDescription>
      </Alert>
    );
  }

  const copy = {
    interrupted: [
      'Last backup did not finish',
      `Timer fired ${relativeTime(status.timer_last_fired)} but no status was written.`,
    ],
    failed:
      status.last_exit_code !== 0
        ? ['Last backup failed', `Exited with code ${status.last_exit_code}.`]
        : ['A backup step failed', 'The snapshot was taken, but a step of the run failed. See Last run.'],
    stale: [
      'Backups are stale',
      lastRun ? `Last status ${lastRun}.` : 'No backup has reported yet.',
    ],
  }[health];

  return (
    <Alert className={health === 'stale' ? 'border-warning/40' : 'border-error/40'}>
      <TriangleAlert className={health === 'stale' ? 'text-warning' : 'text-error'} />
      <AlertTitle>{copy[0]}</AlertTitle>
      <AlertDescription>{copy[1]}</AlertDescription>
    </Alert>
  );
}

function Step({ label, step }: { label: string; step: StepStatus | null | undefined }) {
  if (!step) return null;
  return (
    <div className='flex items-center justify-between px-4 py-2.5 text-sm'>
      <span>{label}</span>
      {step.ok ? (
        <span className='flex items-center gap-1 text-xs text-success'>
          <CircleCheck className='size-3.5' /> {step.message ?? 'OK'}
        </span>
      ) : (
        <span className='flex items-center gap-1 text-xs text-error'>
          <CircleX className='size-3.5' /> {step.message ?? 'Failed'}
        </span>
      )}
    </div>
  );
}

export function BackupsPage() {
  const nodeCount = useNodeStore((s) => s.nodes.length);
  const [nodeId, setNodeId, candidates] = useNodeScope('backups', { capability: 'backups' });
  const meta = useNodeMeta(nodeId);
  const { data: status, isLoading, error } = useBackups(nodeId);
  useTick(60_000);
  const [restoring, setRestoring] = useState<BackupSnapshotInfo | null>(null);

  const enabled = meta?.capabilities.backups ?? false;

  const body = () => {
    if (nodeCount === 0) return <NoNodesState />;
    if (!nodeId || (meta?.status === 'online' && !enabled)) {
      return (
        <EmptyState
          size='page'
          icon={Archive}
          title='Backups not configured'
          description='This agent is not reporting backup status.'
          setup={SETUP.backups}
        />
      );
    }
    if (nodeId && (meta?.status === 'offline' || meta?.status === 'unauthorized')) {
      return <NodeUnavailable nodeId={nodeId} />;
    }
    if (error) {
      return (
        <EmptyState
          size='page'
          icon={TriangleAlert}
          title='Could not load backup status'
        />
      );
    }
    if (isLoading || !status) {
      return (
        <>
          <Skeleton className='h-14' />
          <StatRow>
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className='h-18' />
            ))}
          </StatRow>
        </>
      );
    }

    const r = status.retention;
    return (
      <>
        <HealthAlert status={status} />

        <StatRow>
          <StatCard
            icon={HardDrive}
            label='Repository'
            value={
              status.repo_size_bytes != null
                ? formatBytes(status.repo_size_bytes)
                : NO_VALUE
            }
            sublabel={status.repo_label}
          />
          <StatCard icon={Layers} label='Snapshots' value={status.snapshot_count} />
          <StatCard
            icon={History}
            label='Retention'
            value={r ? `${r.daily}d ${r.weekly}w ${r.monthly}m` : NO_VALUE}
          />
          <StatCard
            icon={CalendarClock}
            label='Next run'
            value={relativeTime(status.next_run) ?? NO_VALUE}
          />
        </StatRow>

        <div className='grid gap-4 @4xl:grid-cols-[1fr_2fr]'>
          <div className='flex min-w-0 flex-col gap-4'>
            <Section title='Last run' contentClassName='divide-y'>
              <Step label='Database dumps' step={status.postgres_dump} />
              <Step label='Copy to second disk' step={status.state_copy} />
              <Step
                label={status.offsite_label ? `Offsite copy to ${status.offsite_label}` : 'Offsite copy'}
                step={status.offsite_copy}
              />
              {status.offsite_copy === null && (
                <div data-no-offsite className='flex items-center justify-between gap-2 px-4 py-2.5 text-sm'>
                  <span>Offsite copy</span>
                  <span className='flex items-center gap-1 text-xs text-muted-foreground'>
                    No copy leaves this machine
                    <SetupHint info={SETUP.offsite} label='How to add one' />
                  </span>
                </div>
              )}
              <Step label='Free space' step={status.space_check} />
              <Step label='Heartbeat' step={status.heartbeat} />
              <div className='flex items-center justify-between px-4 py-2.5 text-sm'>
                <span>Exit code</span>
                <Badge
                  variant='outline'
                  className={`font-mono text-2xs ${status.last_exit_code === 0 ? 'text-success' : 'text-error'}`}
                >
                  {status.last_exit_code ?? NO_VALUE}
                </Badge>
              </div>
            </Section>
            <RequestsSection requests={status.requests ?? []} />
            <RestoreTestSection nodeId={nodeId} status={status} />
          </div>

          <Section title='Snapshots' count={status.snapshots.length || undefined}>
            {status.snapshots.length === 0 ? (
              <EmptyState size='inline' icon={Layers} title='No snapshots' />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow className='hover:bg-transparent'>
                    <TableHead className='h-9 text-xs'>ID</TableHead>
                    <TableHead className='h-9 text-xs'>Taken</TableHead>
                    <TableHead className='h-9 text-xs'>Tags</TableHead>
                    <TableHead className='h-9 text-xs'>Paths</TableHead>
                    <TableHead className='h-9 text-xs'>
                      <span className='sr-only'>Restore</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {status.snapshots.map((s) => (
                    <TableRow key={s.id}>
                      <TableCell className='selectable font-mono text-xs'>
                        {s.short_id}
                      </TableCell>
                      <TableCell className='whitespace-nowrap' title={s.time}>
                        {relativeTime(s.time) ?? NO_VALUE}
                      </TableCell>
                      <TableCell>
                        <div className='flex flex-wrap gap-1'>
                          {s.tags.map((t) => (
                            <Badge key={t} variant='secondary' className='text-2xs'>
                              {t}
                            </Badge>
                          ))}
                        </div>
                      </TableCell>
                      <TableCell
                        className='max-w-64 truncate font-mono text-xs text-muted-foreground'
                        title={s.paths.join('\n')}
                      >
                        {s.paths.join(', ')}
                      </TableCell>
                      <TableCell className='text-right'>
                        <Button
                          variant='ghost'
                          size='sm'
                          aria-label={`Restore from ${s.short_id}`}
                          onClick={() => setRestoring(s)}
                        >
                          <RotateCcw /> Restore
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Section>
        </div>
        {restoring && (
          <RestoreGuide
            open
            onOpenChange={(open) => !open && setRestoring(null)}
            snapshot={restoring}
            databases={status.databases ?? []}
          />
        )}
      </>
    );
  };

  return (
    <>
      <PageHeader
        title='Backups'
        actions={
          <>
            <NodeSelect value={nodeId} onChange={setNodeId} candidates={candidates} />
            <BackUpNowButton nodeId={nodeId} status={status} />
          </>
        }
      />
      {body()}
    </>
  );
}
