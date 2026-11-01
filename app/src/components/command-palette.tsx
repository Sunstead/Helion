import { useMemo, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import {
  ArrowUpRight,
  ChevronLeft,
  CircleArrowDown,
  Copy,
  Info,
  Logs,
  PanelLeft,
  Play,
  Power,
  Plus,
  RefreshCw,
  RotateCcw,
  Square,
} from 'lucide-react';
import { useAppUpdate } from '@/stores/app-update';
import { isDesktop } from '@/lib/platform';
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from '@sunstead/ui/components/command';
import { useSidebar } from '@sunstead/ui/components/resizable-sidebar';
import { useUiStore } from '@/stores/ui';
import { nodeDisplayName, useNodeStore } from '@/stores/nodes';
import { useContainersStore } from '@/stores/containers';
import { useContainerActions, useWol, useWolActions } from '@/api/queries';
import { PAGES } from '@/lib/navigation';
import { ServiceGroup, ServiceInfo, userFacingGroups } from '@/lib/services';
import { serviceHref } from '@/lib/agent-url';
import { openExternal } from '@/lib/open-external';
import { copyText } from '@/lib/clipboard';
import { ServiceIcon } from '@/lib/service-icons';
import { ContainerInfo } from '@/generated/ContainerInfo';
import { ShortcutKeys } from './hint';
import { Dot } from './dot';
import { NodeAvatar } from './node-planet';
import { containerStateVariant } from './container-columns';

type Target = { kind: 'container'; nodeId: string; container: ContainerInfo };

function ContainerPage({
  target,
  done,
}: {
  target: Target;
  done: () => void;
}) {
  const navigate = useNavigate();
  const canAct = useNodeStore((s) => s.meta[target.nodeId]?.capabilities.container_actions ?? false);
  const { run } = useContainerActions(target.nodeId);
  const c = target.container;
  const running = c.state === 'running';

  const act = (action: 'start' | 'stop' | 'restart') => {
    done();
    void run(c.id, action, c.name);
  };

  return (
    <CommandGroup heading={c.name}>
      <CommandItem
        onSelect={() => {
          done();
          void navigate({ to: '/logs', search: { node: target.nodeId, container: c.id } });
        }}
      >
        <Logs /> View logs
      </CommandItem>
      {canAct && !running && (
        <CommandItem onSelect={() => act('start')}>
          <Play /> Start
        </CommandItem>
      )}
      {canAct && running && (
        <>
          <CommandItem onSelect={() => act('restart')}>
            <RotateCcw /> Restart
          </CommandItem>
          <CommandItem onSelect={() => act('stop')}>
            <Square /> Stop
          </CommandItem>
        </>
      )}
      <CommandItem
        onSelect={() => {
          done();
          void copyText(c.id, 'Container ID copied');
        }}
      >
        <Copy /> Copy ID
      </CommandItem>
    </CommandGroup>
  );
}

type PaletteService = {
  group: ServiceGroup;
  instance: ServiceInfo;
  url: string | null;
  multiple: boolean;
};

export function CommandPalette() {
  const open = useUiStore((s) => s.paletteOpen);
  const setOpen = useUiStore((s) => s.setPaletteOpen);
  const setAddNodeOpen = useUiStore((s) => s.setAddNodeOpen);
  const navigate = useNavigate();
  const { toggleSidebar } = useSidebar();

  const nodes = useNodeStore((s) => s.nodes);
  const meta = useNodeStore((s) => s.meta);
  const wol = useWol();
  const { wake } = useWolActions();
  const wakeable = wol.items.filter(
    (i) => meta[i.nodeId]?.capabilities.wol_actions && i.state !== 'awake' && i.state !== 'waking',
  );
  const reconnect = useNodeStore((s) => s.reconnect);
  const nodeContainers = useContainersStore((s) => s.nodeContainers);
  const allGroups = useContainersStore((s) => s.groups);

  const [target, setTarget] = useState<Target | null>(null);
  const [search, setSearch] = useState('');

  // One entry per service: per address when it has one (a service on two
  // nodes behind one URL is one entry), else per node, opening its logs.
  const services = useMemo(
    () =>
      userFacingGroups(allGroups).flatMap((g): PaletteService[] =>
        g.urls.length > 0
          ? g.urls.map((url) => ({
              group: g,
              instance: g.instances.find((i) => i.url === url) ?? g.instances[0],
              url,
              multiple: g.urls.length > 1,
            }))
          : g.instances.map((instance) => ({
              group: g,
              instance,
              url: null,
              multiple: g.instances.length > 1,
            })),
      ),
    [allGroups],
  );
  const containers = useMemo(
    () =>
      Object.entries(nodeContainers).flatMap(([nodeId, list]) =>
        list.map((container) => ({ nodeId, container })),
      ),
    [nodeContainers],
  );

  const close = () => {
    setOpen(false);
    setTarget(null);
    setSearch('');
  };
  const go = (fn: () => void) => () => {
    close();
    fn();
  };
  const nameOf = (id: string) => {
    const n = nodes.find((x) => x.id === id);
    return n ? nodeDisplayName(n) : '';
  };

  return (
    <CommandDialog
      open={open}
      onOpenChange={(o) => (o ? setOpen(true) : close())}
      title='Command palette'
      description='Search pages, nodes, services and containers'
      className='sm:max-w-xl'
    >
      {/* The dialog is already tinted glass; a second bg-popover hides the blur. */}
      <Command loop className='bg-transparent'>
        <CommandInput
          value={search}
          onValueChange={setSearch}
          placeholder={target ? `Actions for ${target.container.name}` : 'Search or run a command'}
          onKeyDown={(e) => {
            if (e.key === 'Backspace' && !search && target) {
              e.preventDefault();
              setTarget(null);
            }
          }}
        />
        <CommandList className='max-h-96'>
          <CommandEmpty>No results</CommandEmpty>

          {target ? (
            <>
              <CommandGroup>
                <CommandItem onSelect={() => setTarget(null)}>
                  <ChevronLeft /> Back
                </CommandItem>
              </CommandGroup>
              <CommandSeparator />
              <ContainerPage target={target} done={close} />
            </>
          ) : (
            <>
              <CommandGroup heading='Go to'>
                {PAGES.map((p) => (
                  <CommandItem
                    key={p.path}
                    value={`go ${p.label}`}
                    onSelect={go(() => void navigate({ to: p.path }))}
                  >
                    <p.icon /> {p.label}
                    <CommandShortcut>
                      <ShortcutKeys id={p.shortcut} />
                    </CommandShortcut>
                  </CommandItem>
                ))}
              </CommandGroup>

              <CommandGroup heading='Actions'>
                <CommandItem value='add node connect' onSelect={go(() => setAddNodeOpen(true))}>
                  <Plus /> Add node
                </CommandItem>
                <CommandItem value='toggle sidebar' onSelect={go(toggleSidebar)}>
                  <PanelLeft /> Toggle sidebar
                  <CommandShortcut>
                    <ShortcutKeys id='sidebar' />
                  </CommandShortcut>
                </CommandItem>
                {isDesktop() && (
                  <CommandItem
                    value='check for app updates helion version'
                    onSelect={go(() => void useAppUpdate.getState().check({ manual: true }))}
                  >
                    <CircleArrowDown /> Check for app updates
                  </CommandItem>
                )}
              </CommandGroup>

              {wakeable.length > 0 && (
                <CommandGroup heading='Wake'>
                  {wakeable.map((i) => (
                    <CommandItem
                      key={`${i.nodeId}:${i.target.id}`}
                      value={`wake ${i.target.name} wol`}
                      onSelect={go(() => void wake(i.nodeId, i.target.id, i.target.name))}
                    >
                      <Power /> Wake {i.target.name}
                    </CommandItem>
                  ))}
                </CommandGroup>
              )}

              {nodes.length > 0 && (
                <CommandGroup heading='Nodes'>
                  {nodes.map((n) => (
                    <CommandItem
                      key={n.id}
                      value={`node ${nodeDisplayName(n)} ${n.url} ${n.agentName ?? ''}`}
                      onSelect={go(() => void navigate({ to: '/nodes/$nodeId', params: { nodeId: n.id } }))}
                    >
                      <NodeAvatar nodeId={n.id} size={16} />
                      {nodeDisplayName(n)}
                      <span className='truncate text-xs text-muted-foreground'>{n.url}</span>
                      {meta[n.id]?.status !== 'online' && (
                        <CommandShortcut>
                          <button
                            type='button'
                            className='flex items-center gap-1 hover:text-foreground'
                            onClick={(e) => {
                              e.stopPropagation();
                              reconnect(n.id);
                            }}
                          >
                            <RefreshCw className='size-3' /> Reconnect
                          </button>
                        </CommandShortcut>
                      )}
                    </CommandItem>
                  ))}
                </CommandGroup>
              )}

              {services.length > 0 && (
                <CommandGroup heading='Services'>
                  {services.map(({ group, instance, url, multiple }) => {
                    const href = serviceHref(url);
                    return (
                      <CommandItem
                        key={`${group.key}:${url ?? instance.nodeId}`}
                        value={`service ${group.name} ${group.key} ${url ?? ''} ${multiple ? nameOf(instance.nodeId) : ''}`}
                        onSelect={go(() =>
                          href
                            ? void openExternal(href)
                            : void navigate({
                                to: '/logs',
                                search: { node: instance.nodeId, container: instance.containers[0]?.id },
                              }),
                        )}
                      >
                        <ServiceIcon service={group.key} icon={group.icon} size={16} />
                        {group.name}
                        {nodes.length > 1 && multiple && (
                          <span className='text-xs text-muted-foreground'>{nameOf(instance.nodeId)}</span>
                        )}
                        <CommandShortcut className='tracking-normal'>
                          {href ? (
                            <span className='flex items-center gap-1'>
                              Open <ArrowUpRight className='size-3' />
                            </span>
                          ) : (
                            'Logs'
                          )}
                        </CommandShortcut>
                      </CommandItem>
                    );
                  })}
                </CommandGroup>
              )}

              {containers.length > 0 && (
                <CommandGroup heading='Containers'>
                  {containers.map(({ nodeId, container }) => (
                    <CommandItem
                      key={`${nodeId}:${container.id}`}
                      value={`container ${container.name} ${container.image} ${container.id.slice(0, 12)}`}
                      onSelect={() => {
                        setTarget({ kind: 'container', nodeId, container });
                        setSearch('');
                      }}
                    >
                      <Dot variant={containerStateVariant(container.state)} />
                      {container.name}
                      <span className='truncate font-mono text-2xs text-muted-foreground'>
                        {container.image}
                      </span>
                      <CommandShortcut className='tracking-normal'>
                        <Info className='size-3' />
                      </CommandShortcut>
                    </CommandItem>
                  ))}
                </CommandGroup>
              )}
            </>
          )}
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
