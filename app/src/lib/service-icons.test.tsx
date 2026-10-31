import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { ServiceIcon } from '@/lib/service-icons';
import { resetAgentIcons } from '@/lib/agent-icon';
import * as nodes from '@/stores/nodes';
import type { NodeConnection } from '@/api/connection';

function connection(getIcon: (name: string, theme: string) => Promise<Blob>) {
  vi.spyOn(nodes, 'getConnection').mockImplementation(
    (id) => (id === 'n1' ? ({ client: { getIcon } } as unknown as NodeConnection) : null),
  );
}

beforeEach(() => {
  URL.createObjectURL = vi.fn(() => 'blob:icon');
  URL.revokeObjectURL = vi.fn();
});
afterEach(() => {
  resetAgentIcons();
  vi.restoreAllMocks();
});

describe('ServiceIcon', () => {
  it('draws a Sunstead product itself, whatever an agent says', () => {
    const getIcon = vi.fn();
    connection(getIcon);
    const { container } = render(<ServiceIcon service='cosmos' icon={{ nodeId: 'n1', name: 'cosmos' }} />);
    expect(container.querySelector('svg')).toBeTruthy();
    expect(getIcon).not.toHaveBeenCalled();
  });

  it('shows the generic icon until the one from the agent arrives', async () => {
    const getIcon = vi.fn(async () => new Blob(['<svg/>'], { type: 'image/svg+xml' }));
    connection(getIcon);
    const { container } = render(<ServiceIcon service='jellyfin' icon={{ nodeId: 'n1', name: 'jellyfin' }} />);
    expect(container.querySelector('svg')).toBeTruthy();

    await waitFor(() => expect(container.querySelector('img')).toHaveAttribute('src', 'blob:icon'));
    expect(getIcon).toHaveBeenCalledWith('jellyfin', 'dark');
    expect(container.querySelector('svg')).toBeNull();
  });

  it('falls back to the generic icon when the agent has none', async () => {
    const getIcon = vi.fn(async () => {
      throw new Error('404');
    });
    connection(getIcon);
    const { container } = render(<ServiceIcon service='mystery' icon={{ nodeId: 'n1', name: 'mystery' }} />);
    await waitFor(() => expect(getIcon).toHaveBeenCalled());
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('svg')).toBeTruthy();
    expect(screen.queryByRole('img')).toBeNull();
  });

  it('asks for nothing when there is no icon to ask for', () => {
    const getIcon = vi.fn();
    connection(getIcon);
    render(<ServiceIcon service='mystery' />);
    expect(getIcon).not.toHaveBeenCalled();
  });
});
