import { describe, expect, test } from 'bun:test';
import {
  MetricsRegistry,
  TelemetryFlusher,
  type MetricsSink,
} from '../src/telemetry.ts';

describe('MetricsRegistry', () => {
  test('counters increment and aggregate by labelset', () => {
    const r = new MetricsRegistry();
    r.inc('mm_orders_placed_total', 'orders placed', { market: 'sol', side: 'long' });
    r.inc('mm_orders_placed_total', 'orders placed', { market: 'sol', side: 'long' });
    r.inc('mm_orders_placed_total', 'orders placed', { market: 'sol', side: 'short' });
    const out = r.render();
    expect(out).toContain('mm_orders_placed_total{market="sol",side="long"} 2');
    expect(out).toContain('mm_orders_placed_total{market="sol",side="short"} 1');
  });

  test('gauges replace prior value', () => {
    const r = new MetricsRegistry();
    r.set('mm_inventory_lots', 'inventory', { market: 'sol' }, 100);
    r.set('mm_inventory_lots', 'inventory', { market: 'sol' }, -50);
    const out = r.render();
    expect(out).toContain('mm_inventory_lots{market="sol"} -50');
    expect(out).not.toContain('mm_inventory_lots{market="sol"} 100');
  });

  test('render emits HELP and TYPE lines once per metric name', () => {
    const r = new MetricsRegistry();
    r.inc('foo', 'help text');
    r.inc('foo', 'help text', { tag: 'a' });
    const out = r.render();
    const helpCount = (out.match(/# HELP foo/g) ?? []).length;
    const typeCount = (out.match(/# TYPE foo/g) ?? []).length;
    expect(helpCount).toBe(1);
    expect(typeCount).toBe(1);
  });

  test('label values escape special characters', () => {
    const r = new MetricsRegistry();
    r.set('x', 'help', { msg: 'has "quotes" and \\ slashes' }, 1);
    const out = r.render();
    expect(out).toContain('msg="has \\"quotes\\" and \\\\ slashes"');
  });
});

describe('TelemetryFlusher', () => {
  test('flushes once on demand to the sink', async () => {
    const r = new MetricsRegistry();
    r.inc('x', 'h');
    let captured = '';
    const sink: MetricsSink = { flush: (text) => { captured = text; } };
    const f = new TelemetryFlusher(r, sink, 60_000);
    await f.flushOnce();
    expect(captured).toContain('x ');
  });
});
