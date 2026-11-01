import { memo } from 'react';
import { ServiceGroup } from '@/lib/services';
import { ServiceIcon } from '@/lib/service-icons';
import { displayHost, serviceHref } from '@/lib/agent-url';
import { openExternal } from '@/lib/open-external';
import { getServiceStatusDisplay } from '@/lib/service-utils';
import { useNodeStore, nodeDisplayName } from '@/stores/nodes';
import { Button } from '@sunstead/ui/components/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@sunstead/ui/components/dropdown-menu';
import { Dot } from './dot';

const BUTTON_CLASS = 'relative flex h-auto flex-col items-center gap-1.5 p-3';

export const QuickLaunchServiceButton = memo(function QuickLaunchServiceButton({
  group,
}: {
  group: ServiceGroup;
}) {
  const nodes = useNodeStore((s) => s.nodes);
  const status = getServiceStatusDisplay(group.status);
  const face = (
    <>
      <Dot variant={status.dotVariant} className='absolute top-1.5 right-1.5' />
      <ServiceIcon service={group.key} icon={group.icon} size={24} />
      <span className='text-xs truncate max-w-full'>{group.name}</span>
    </>
  );

  // Nodes serving it at different addresses: say which one opens.
  if (group.urls.length > 1) {
    const withUrl = group.instances.filter((s) => s.url);
    return (
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<Button variant='secondary' className={BUTTON_CLASS} title={`Open ${group.name} on...`} />}
        >
          {face}
        </DropdownMenuTrigger>
        <DropdownMenuContent align='center' className='w-56'>
          {withUrl.map((s) => {
            const node = nodes.find((n) => n.id === s.nodeId);
            const href = serviceHref(s.url);
            return (
              <DropdownMenuItem key={s.nodeId} onClick={() => href && void openExternal(href)}>
                <span className='font-medium'>{node ? nodeDisplayName(node) : s.nodeId}</span>
                <span className='truncate text-xs text-muted-foreground'>{displayHost(s.url)}</span>
              </DropdownMenuItem>
            );
          })}
        </DropdownMenuContent>
      </DropdownMenu>
    );
  }

  // Docker labels hold bare hostnames like `portainer.home.example.com`.
  // As an href that is a *relative path*, so the old anchor navigated the app
  // to /portainer.home.example.com instead of opening the service.
  const href = serviceHref(group.url);
  return (
    <Button
      variant='secondary'
      disabled={!href}
      title={href ?? `${group.name} has no helion.service.url label`}
      onClick={() => href && openExternal(href)}
      className={BUTTON_CLASS}
    >
      {face}
    </Button>
  );
});

export default QuickLaunchServiceButton;
