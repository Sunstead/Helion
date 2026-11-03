import { Area, AreaChart, CartesianGrid, Legend, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { ReactNode } from 'react';
import { ChartRow, CompareRow } from '@/lib/metric-series';
import { Section } from './section';

const AXIS = { stroke: 'var(--color-muted-foreground)', fontSize: 11, tickLine: false } as const;

function timeTick(spanMs: number) {
  return (v: number) =>
    new Date(v).toLocaleString([], spanMs > 86_400_000
      ? { month: 'short', day: 'numeric' }
      : { hour: '2-digit', minute: '2-digit' });
}

const TOOLTIP_STYLE = {
  background: 'var(--color-popover)',
  border: '1px solid var(--color-border)',
  borderRadius: 'var(--radius-lg)',
  fontSize: 12,
} as const;

/**
 * The theme's chart palette, one colour per node in node order, plus a dash
 * pattern each: some themes' palettes are shades of one colour.
 */
const NODE_COLORS = [1, 2, 3, 4, 5].map((i) => `var(--chart-${i})`);
const NODE_DASHES = [undefined, '6 3', '2 3', '8 3 2 3'];

/** One metric from several nodes, a line each. */
export function CompareChart({
  title,
  rows,
  nodes,
  format,
  max,
  actions,
  gaps = false,
}: {
  title: string;
  rows: CompareRow[];
  /** In legend order; each id is a key in `rows`. */
  nodes: { id: string; name: string }[];
  format: (v: number) => string;
  max?: number;
  actions?: ReactNode;
  /**
   * Break lines across nulls instead of bridging them. Nodes bucket
   * independently, so their nulls are misalignment; a container's are time
   * it wasn't running. Also sorts the tooltip, largest first.
   */
  gaps?: boolean;
}) {
  const span = rows.length > 1 ? rows[rows.length - 1].ts - rows[0].ts : 0;

  return (
    <Section title={title} actions={actions} contentClassName='aspect-[5/2] min-h-40 p-2 pr-4'>
      <ResponsiveContainer width='100%' height='100%'>
        <LineChart data={rows} margin={{ top: 8, right: 0, bottom: 0, left: 0 }}>
          <CartesianGrid stroke='var(--color-border)' vertical={false} />
          <XAxis
            dataKey='ts'
            type='number'
            scale='time'
            domain={['dataMin', 'dataMax']}
            tickFormatter={timeTick(span)}
            minTickGap={48}
            {...AXIS}
          />
          <YAxis
            domain={[0, max ?? 'auto']}
            tickFormatter={format}
            width={64}
            axisLine={false}
            {...AXIS}
          />
          <Tooltip
            cursor={{ stroke: 'var(--color-border)' }}
            contentStyle={TOOLTIP_STYLE}
            labelFormatter={(v) => new Date(Number(v)).toLocaleString()}
            formatter={(v) => format(Number(v))}
            itemSorter={gaps ? (item) => -Number(item.value ?? 0) : undefined}
          />
          <Legend iconType='plainline' wrapperStyle={{ fontSize: 12 }} />
          {nodes.map((n, i) => (
            <Line
              key={n.id}
              type='monotone'
              dataKey={n.id}
              name={n.name}
              stroke={NODE_COLORS[i % NODE_COLORS.length]}
              strokeDasharray={NODE_DASHES[i % NODE_DASHES.length]}
              strokeWidth={1.5}
              dot={false}
              connectNulls={!gaps}
              isAnimationActive={false}
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
    </Section>
  );
}

/** Average as a line, peak as a band beneath it so averaged-away spikes show. */
export function MetricChart({
  title,
  rows,
  color,
  format,
  max,
}: {
  title: string;
  rows: ChartRow[];
  color: string;
  format: (v: number) => string;
  max?: number;
}) {
  const span = rows.length > 1 ? rows[rows.length - 1].ts - rows[0].ts : 0;

  return (
    <Section title={title} contentClassName='aspect-[5/2] min-h-40 p-2 pr-4'>
      <ResponsiveContainer width='100%' height='100%'>
        <AreaChart data={rows} margin={{ top: 8, right: 0, bottom: 0, left: 0 }}>
          <CartesianGrid stroke='var(--color-border)' vertical={false} />
          <XAxis
            dataKey='ts'
            type='number'
            scale='time'
            domain={['dataMin', 'dataMax']}
            tickFormatter={timeTick(span)}
            minTickGap={48}
            {...AXIS}
          />
          <YAxis
            domain={[0, max ?? 'auto']}
            tickFormatter={format}
            width={64}
            axisLine={false}
            {...AXIS}
          />
          <Tooltip
            cursor={{ stroke: 'var(--color-border)' }}
            contentStyle={TOOLTIP_STYLE}
            labelFormatter={(v) => new Date(Number(v)).toLocaleString()}
            formatter={(v, name) => [format(Number(v)), name === 'peak' ? 'Peak' : 'Average']}
          />
          <Area
            type='monotone'
            dataKey='peak'
            stroke='none'
            fill={`color-mix(in srgb, ${color} 16%, transparent)`}
            isAnimationActive={false}
          />
          <Area
            type='monotone'
            dataKey='value'
            stroke={color}
            strokeWidth={1.5}
            fill={`color-mix(in srgb, ${color} 8%, transparent)`}
            isAnimationActive={false}
          />
        </AreaChart>
      </ResponsiveContainer>
    </Section>
  );
}
