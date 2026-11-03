import { describe, expect, it } from 'vitest';
import { MetricSeries } from '@/generated/MetricSeries';
import { ContainerSeries } from '@/generated/ContainerSeries';
import { containerChartRows, containerRows, mergeSeries } from './metric-series';

function series(step: number, ts: number[], cpu: number[]): MetricSeries {
  const zeros = ts.map(() => 0);
  return {
    step_secs: step,
    from: ts[0] ?? 0,
    to: ts[ts.length - 1] ?? 0,
    ts,
    cpu_pct: cpu,
    cpu_pct_max: cpu,
    mem_used_bytes: zeros,
    swap_used_bytes: zeros,
    net_rx_bps: zeros,
    net_rx_bps_max: zeros,
    net_tx_bps: zeros,
    net_tx_bps_max: zeros,
    disk_read_bps: zeros,
    disk_read_bps_max: zeros,
    disk_write_bps: zeros,
    disk_write_bps_max: zeros,
    load1: zeros,
    n: zeros,
  } as unknown as MetricSeries;
}

describe('mergeSeries', () => {
  it('lines up nodes whose samples are a little apart', () => {
    const rows = mergeSeries(
      [
        { nodeId: 'jupiter', series: series(60, [60, 120], [10, 20]) },
        { nodeId: 'pluto', series: series(60, [61, 119], [1, 2]) },
      ],
      'cpu_pct',
    );
    expect(rows).toEqual([
      { ts: 60_000, jupiter: 10, pluto: 1 },
      { ts: 120_000, jupiter: 20, pluto: 2 },
    ]);
  });

  it('leaves a gap where a node has nothing', () => {
    const rows = mergeSeries(
      [
        { nodeId: 'jupiter', series: series(60, [60, 120], [10, 20]) },
        { nodeId: 'pluto', series: series(60, [120], [2]) },
      ],
      'cpu_pct',
    );
    expect(rows[0]).toEqual({ ts: 60_000, jupiter: 10, pluto: null });
  });

  it('snaps to the coarsest step', () => {
    const rows = mergeSeries(
      [
        { nodeId: 'jupiter', series: series(300, [300], [10]) },
        { nodeId: 'pluto', series: series(60, [240, 300], [1, 2]) },
      ],
      'cpu_pct',
    );
    expect(rows).toEqual([{ ts: 300_000, jupiter: 10, pluto: 2 }]);
  });
});

describe('container history', () => {
  const nulls = [null, null];
  const s: ContainerSeries = {
    step_secs: 60,
    from: 0,
    to: 60,
    ts: [0, 60],
    containers: [
      { name: 'web', service: 'web', cpu_pct: [10, 20], cpu_pct_max: [15, 40], mem_used_bytes: nulls, mem_used_bytes_max: nulls, net_rx_bps: nulls, net_tx_bps: nulls, disk_read_bps: nulls, disk_write_bps: nulls },
      { name: 'db', service: null, cpu_pct: [null, 5], cpu_pct_max: [null, 5], mem_used_bytes: nulls, mem_used_bytes_max: nulls, net_rx_bps: nulls, net_tx_bps: nulls, disk_read_bps: nulls, disk_write_bps: nulls },
    ],
  };

  it('puts every container on the shared axis, null where it did not run', () => {
    expect(containerRows(s, 'cpu_pct')).toEqual([
      { ts: 0, web: 10, db: null },
      { ts: 60_000, web: 20, db: 5 },
    ]);
  });

  it('gives one container its average and peak', () => {
    expect(containerChartRows(s, 'web', 'cpu_pct', 'cpu_pct_max')).toEqual([
      { ts: 0, value: 10, peak: 15 },
      { ts: 60_000, value: 20, peak: 40 },
    ]);
    expect(containerChartRows(s, 'gone', 'cpu_pct')).toEqual([]);
  });
});
