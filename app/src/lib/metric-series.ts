import { ContainerHistory } from '@/generated/ContainerHistory';
import { ContainerSeries } from '@/generated/ContainerSeries';
import { MetricSeries } from '@/generated/MetricSeries';

/** `null` where there's no bucket, so the chart breaks the line. */
export interface ChartRow {
  ts: number;
  value: number | null;
  peak: number | null;
}

export type NumericKey = {
  [K in keyof MetricSeries]: MetricSeries[K] extends number[] ? K : never;
}[keyof MetricSeries];

/** Zips the agent's columnar response into recharts rows. */
export function toRows(series: MetricSeries | undefined, key: NumericKey, peakKey: NumericKey): ChartRow[] {
  if (!series) return [];
  const values = series[key];
  const peaks = series[peakKey];
  return series.ts.map((ts, i) => ({
    ts: ts * 1000,
    value: values[i] ?? 0,
    peak: peaks[i] ?? values[i] ?? 0,
  }));
}

/** A row of a comparison chart: the time, then one value per node id. */
export type CompareRow = { ts: number } & Record<string, number | null>;

/**
 * Several nodes' series for one metric, merged into rows a single chart can
 * draw (one line per node). Agents bucket independently, so their timestamps
 * needn't line up: each is snapped to the coarsest `step_secs` among them, and a node
 * with nothing in a bucket gets null there (the line bridges the gap).
 */
export function mergeSeries(
  inputs: { nodeId: string; series: MetricSeries }[],
  key: NumericKey,
): CompareRow[] {
  const step = Math.max(1, ...inputs.map((i) => i.series.step_secs));

  const rows = new Map<number, CompareRow>();
  for (const { nodeId, series } of inputs) {
    const values = series[key];
    series.ts.forEach((ts, i) => {
      const bucket = Math.round(ts / step) * step * 1000;
      let row = rows.get(bucket);
      if (!row) {
        row = { ts: bucket } as CompareRow;
        for (const other of inputs) row[other.nodeId] = null;
        rows.set(bucket, row);
      }
      // The latest sample in the bucket wins; they're the same resolution.
      row[nodeId] = values[i] ?? null;
    });
  }
  return [...rows.values()].sort((a, b) => a.ts - b.ts);
}

export type ContainerKey = {
  [K in keyof ContainerHistory]: ContainerHistory[K] extends (number | null)[] ? K : never;
}[keyof ContainerHistory];

/**
 * One metric for every container in the response, a column each keyed by
 * container name. The agent already puts them on one time axis.
 */
export function containerRows(series: ContainerSeries | undefined, key: ContainerKey): CompareRow[] {
  if (!series) return [];
  return series.ts.map((ts, i) => {
    const row = { ts: ts * 1000 } as CompareRow;
    for (const c of series.containers) row[c.name] = c[key][i] ?? null;
    return row;
  });
}

/** One container's metric as average and peak rows, for a `MetricChart`. */
export function containerChartRows(
  series: ContainerSeries | undefined,
  name: string,
  key: ContainerKey,
  peakKey: ContainerKey = key,
): ChartRow[] {
  const c = series?.containers.find((x) => x.name === name);
  if (!series || !c) return [];
  return series.ts.map((ts, i) => ({
    ts: ts * 1000,
    value: c[key][i] ?? null,
    peak: c[peakKey][i] ?? c[key][i] ?? null,
  }));
}
