import { ComponentType, SVGProps, useContext } from 'react';
import { Server } from 'lucide-react';
import { isProduct } from '@sunstead/ui/products';
import { ProductIcon } from '@sunstead/ui/product-icon';
import { ThemeContext } from '@sunstead/ui/use-theme';

import { useAgentIcon } from '@/lib/agent-icon';
import type { IconRef } from '@/lib/services';

type SvgComponent = ComponentType<SVGProps<SVGSVGElement>>;

/**
 * Icons that ship with the app, by file name: what shows before an agent has
 * sent its own, on a node that can't reach the icon source, or offline. The
 * agents find icons for everything by themselves, so nothing needs adding
 * here; `local/` is for a service an agent gets wrong.
 */
const BUNDLED = import.meta.glob<{ default: SvgComponent }>('@/assets/icons/**/*.svg', {
  query: '?react',
  eager: true,
});

const bundled = new Map<string, SvgComponent>(
  Object.entries(BUNDLED).map(([file, mod]) => [
    // `.../icons/gitea.svg` and `.../icons/local/gitea.svg` are both `gitea`.
    file.replace(/^.*\/icons\//, '').replace(/^local\//, '').replace(/\.svg$/, ''),
    mod.default,
  ]),
);

/** Names a service goes by that are another's. Before the rename Helion was Cosmos. */
const ALIASES: Record<string, string> = {
  'helion-agent': 'helion',
  cosmos: 'helion',
  'cosmos-agent': 'helion',
};

function bundledIcon(key: string, size: number, props: SVGProps<SVGSVGElement>) {
  const Icon = bundled.get(key);
  return Icon ? <Icon width={size} height={size} {...props} /> : null;
}

interface ServiceIconProps extends SVGProps<SVGSVGElement> {
  service: string;
  /** The agent's icon for this service, when it has found one. */
  icon?: IconRef | null;
  size?: number;
}

/**
 * A service's icon: a Sunstead product's own, else the agent's (found from its
 * image and labels), else one bundled with the app, else a generic server.
 */
export function ServiceIcon({ service, icon = null, size = 24, ...props }: ServiceIconProps) {
  // Without a provider (a test, a bare story) there's no scheme to follow.
  const scheme = useContext(ThemeContext)?.resolved.scheme ?? 'dark';
  const key = service.toLowerCase();
  const product = ALIASES[key] ?? key;
  const fromAgent = useAgentIcon(isProduct(product) ? null : icon, scheme);

  if (isProduct(product)) {
    return <ProductIcon product={product} size={size} {...props} />;
  }
  if (fromAgent) {
    return (
      <img
        src={fromAgent}
        width={size}
        height={size}
        alt=''
        draggable={false}
        className={props.className}
        style={props.style}
      />
    );
  }

  return bundledIcon(key, size, props) ?? <Server size={size} />;
}
