// Order simulator tests — mirror the Rust matcher's property tests
// where possible to verify the TS port produces identical results.

import { describe, expect, test } from 'bun:test';
import {
  fillForOrder,
  simulateBatchClearing,
  type SimOrder,
} from '../src/order-simulator.ts';

function ord(opts: {
  id: string;
  trader?: string;
  side: 'long' | 'short';
  orderType?: SimOrder['orderType'];
  sizeLots: number;
  limitTicks: number;
  seq?: number;
}): SimOrder {
  return {
    id: opts.id,
    trader: opts.trader ?? `t_${opts.id}`,
    side: opts.side,
    orderType: opts.orderType ?? 'limit',
    sizeLots: opts.sizeLots,
    limitTicks: opts.limitTicks,
    seq: opts.seq ?? 0,
  };
}

describe('simulateBatchClearing', () => {
  test('empty batch returns no fills', () => {
    const r = simulateBatchClearing([], 100);
    expect(r.clearingVolumeLots).toBe(0);
    expect(r.fills).toHaveLength(0);
    expect(r.clearingPriceTicks).toBe(100);
  });

  test('non-crossing book has no fills', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b', side: 'long', sizeLots: 1, limitTicks: 99 }),
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 101 }),
      ],
      100,
    );
    expect(r.clearingVolumeLots).toBe(0);
  });

  test('crossing produces a uniform-price fill', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b', side: 'long', sizeLots: 1, limitTicks: 101 }),
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 99 }),
      ],
      100,
    );
    expect(r.clearingVolumeLots).toBe(1);
    expect(r.fills).toHaveLength(1);
    expect(r.fills[0]!.priceTicks).toBeGreaterThanOrEqual(99);
    expect(r.fills[0]!.priceTicks).toBeLessThanOrEqual(101);
  });

  test('clearing price favors prior mark when ties exist', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b1', side: 'long', sizeLots: 1, limitTicks: 101 }),
        ord({ id: 'b2', side: 'long', sizeLots: 1, limitTicks: 100 }),
        ord({ id: 's1', side: 'short', sizeLots: 1, limitTicks: 100 }),
        ord({ id: 's2', side: 'short', sizeLots: 1, limitTicks: 99 }),
      ],
      100,
    );
    expect(r.clearingPriceTicks).toBe(100);
  });

  test('liquidation orders fill before regular takers', () => {
    const r = simulateBatchClearing(
      [
        ord({
          id: 't',
          side: 'long',
          orderType: 'taker',
          sizeLots: 1,
          limitTicks: 105,
          trader: 'A',
          seq: 0,
        }),
        ord({
          id: 'l',
          side: 'long',
          orderType: 'liquidation',
          sizeLots: 1,
          limitTicks: 105,
          trader: 'L',
          seq: 1,
        }),
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 95 }),
      ],
      100,
    );
    expect(r.fills[0]!.takerTrader).toBe('L');
  });

  test('FIFO within priority class', () => {
    const r = simulateBatchClearing(
      [
        ord({
          id: 'late',
          side: 'long',
          orderType: 'taker',
          sizeLots: 1,
          limitTicks: 105,
          trader: 'A',
          seq: 100,
        }),
        ord({
          id: 'early',
          side: 'long',
          orderType: 'taker',
          sizeLots: 1,
          limitTicks: 105,
          trader: 'B',
          seq: 50,
        }),
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 95 }),
      ],
      100,
    );
    expect(r.fills[0]!.takerTrader).toBe('B');
  });

  test('self-trade prevented', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b', side: 'long', sizeLots: 1, limitTicks: 105, trader: 'X' }),
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 95, trader: 'X' }),
      ],
      100,
    );
    expect(r.fills.length).toBe(0);
  });

  test('MEV-neutrality: permutation does not change clearing', () => {
    const a = [
      ord({ id: 'b1', side: 'long', sizeLots: 2, limitTicks: 102, seq: 10 }),
      ord({ id: 's1', side: 'short', sizeLots: 2, limitTicks: 98, seq: 20 }),
      ord({ id: 'b2', side: 'long', sizeLots: 1, limitTicks: 101, seq: 30 }),
    ];
    const b = [a[2]!, a[0]!, a[1]!];
    const ra = simulateBatchClearing(a, 100);
    const rb = simulateBatchClearing(b, 100);
    expect(ra.clearingPriceTicks).toBe(rb.clearingPriceTicks);
    expect(ra.clearingVolumeLots).toBe(rb.clearingVolumeLots);
  });

  test('volume maximization', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b1', side: 'long', sizeLots: 5, limitTicks: 100 }),
        ord({ id: 'b2', side: 'long', sizeLots: 5, limitTicks: 99 }),
        ord({ id: 's1', side: 'short', sizeLots: 4, limitTicks: 100 }),
        ord({ id: 's2', side: 'short', sizeLots: 4, limitTicks: 99 }),
      ],
      99.5,
    );
    expect(r.clearingVolumeLots).toBeGreaterThanOrEqual(5);
  });

  test('fillForOrder returns aggregate size + price', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b1', side: 'long', sizeLots: 5, limitTicks: 105, seq: 0 }),
        ord({ id: 's1', side: 'short', sizeLots: 3, limitTicks: 95, seq: 1 }),
        ord({ id: 's2', side: 'short', sizeLots: 2, limitTicks: 95, seq: 2 }),
      ],
      100,
    );
    const myFill = fillForOrder(r, 'b1');
    expect(myFill.sizeLots).toBe(5); // matched against both sells
    expect(myFill.priceTicks).toBeGreaterThan(0);
  });

  test('fillForOrder returns zero for unfilled', () => {
    const r = simulateBatchClearing(
      [
        ord({ id: 'b', side: 'long', sizeLots: 1, limitTicks: 99 }), // doesn't cross
        ord({ id: 's', side: 'short', sizeLots: 1, limitTicks: 101 }),
      ],
      100,
    );
    expect(fillForOrder(r, 'b').sizeLots).toBe(0);
  });
});
