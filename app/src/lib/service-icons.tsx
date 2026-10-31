import { SVGProps, useContext } from 'react';
import { Server } from 'lucide-react';
import { isProduct } from '@sunstead/ui/products';
import { ProductIcon } from '@sunstead/ui/product-icon';
import { ThemeContext } from '@sunstead/ui/use-theme';

import { useAgentIcon } from '@/lib/agent-icon';
import type { IconRef } from '@/lib/services';

/** Names a service goes by that are another's. Before the rename Helion was Cosmos. */
const ALIASES: Record<string, string> = {
  'helion-agent': 'helion',
  cosmos: 'helion',
  'cosmos-agent': 'helion',
};

interface ServiceIconProps extends SVGProps<SVGSVGElement> {
  service: string;
  /** The agent's icon for this service, when it has found one. */
  icon?: IconRef | null;
  size?: number;
}

/**
 * A service's icon: a Sunstead product's own, else the one the agent found
 * from the service's image and labels, else a generic server. Nothing is
 * bundled: the agents find icons for everything, and `helion.icon` fixes one.
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

  return <Server size={size} className={props.className} />;
}
