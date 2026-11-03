import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import React from 'react';
import { ServiceIcon } from '@/lib/service-icons';

const read = (p: string) => readFileSync(path.resolve(__dirname, '../..', p), 'utf8');
const pathData = (svg: string) => / d="([^"]*)"/.exec(svg)?.[1];

const source = read('src/assets/icon_simple.svg');
const favicon = read('public/favicon.svg');
const mark = read('src/assets/icon-mark.svg');
const css = read('node_modules/@sunstead/ui/src/themes/sunstead.css');

describe('generated mark', () => {
  it('matches the Inkscape source (re-run scripts/build-mark.mjs if this fails)', () => {
    const expected = pathData(source)?.split(/\s+/).join(' ');
    expect(expected).toBeTruthy();
    expect(pathData(favicon)).toBe(expected);
    expect(pathData(mark)).toBe(expected);
  });

  it('favicon carries both palettes and switches on the OS scheme', () => {
    expect(favicon).toContain('@media (prefers-color-scheme: dark)');
    for (const colour of ['#3c3c3c', '#000000', '#ffffff', '#a1a1a1']) {
      expect(favicon).toContain(colour);
    }
  });

  it('in-app mark reads theme tokens and ships no document-wide styles', () => {
    expect(mark).toContain('var(--mark-0');
    expect(mark).toContain('var(--mark-1');
    expect(mark).not.toContain('<style');
    expect(mark).not.toContain('prefers-color-scheme');
  });

  it('both base themes define the mark tokens (@sunstead/ui checks every theme)', () => {
    const dark = css.indexOf("[data-theme='sunstead-dark']");
    expect(css.indexOf("[data-theme='sunstead-light']")).toBeLessThan(dark);
    expect(dark).toBeGreaterThan(0);
    for (const token of ['--mark-0:', '--mark-1:']) {
      const hits = [...css.matchAll(new RegExp(token, 'g'))].map((m) => m.index!);
      expect(hits, token).toHaveLength(2);
      expect(hits.some((i) => i < dark), `${token} in the light theme`).toBe(true);
      expect(hits.some((i) => i > dark), `${token} in the dark theme`).toBe(true);
    }
  });
});

describe('ServiceIcon', () => {
  it('renders the Helion app tile at its size, even where CSS sizes svgs', () => {
    const { container } = render(React.createElement(ServiceIcon, { service: 'helion', size: 24 }));
    const svg = container.querySelector('svg')!;
    expect(svg.querySelector('[id^="sunstead-helion"]')).not.toBeNull();
    expect(svg).toHaveAttribute('width', '24');
    // A Button's `[&_svg]:size-4` beats the attributes; only inline wins.
    expect(svg.style.width).toBe('24px');
    expect(svg.style.height).toBe('24px');
  });

  it('treats the old Cosmos names as Helion', () => {
    const { container } = render(React.createElement(ServiceIcon, { service: 'cosmos-agent' }));
    expect(container.querySelector('[id^="sunstead-helion"]')).not.toBeNull();
  });
});
