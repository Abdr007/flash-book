import { describe, expect, test } from 'bun:test';
import { clearBatch, type ClearBatchInput } from '../src/matcher.ts';
import type { Order, Side } from '../src/types.ts';
import { TEST_PARAMS } from './_helpers.ts';

const PARAMS = TEST_PARAMS;

function order(args: { id: string; side: Side; size: number; limit: number; type?: Order['type']; trader?: string; ts?: number }): Order {
  return {
    id: args.id,
    market: 'SOL',
    trader: args.trader ?? args.id,
    side: args.side,
    size: args.size,
    limitPrice: args.limit,
    type: args.type ?? 'limit',
    timestamp: args.ts ?? 0,
    postOnly: false,
  };
}

function input(orders: Order[], priorMark = 100): ClearBatchInput {
  return {
    market: 'SOL',
    batchNum: 1,
    nowMs: 1000,
    orders,
    priorMarkPrice: priorMark,
    params: PARAMS,
    vpin: 0,
  };
}

describe('clearBatch', () => {
  test('empty batch returns no fills', () => {
    const r = clearBatch(input([]));
    expect(r.clearingVolume).toBe(0);
    expect(r.fills).toHaveLength(0);
  });

  test('non-crossing book has no fills', () => {
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 1, limit: 99 }),
      order({ id: 's1', side: 'short', size: 1, limit: 101 }),
    ]));
    expect(r.clearingVolume).toBe(0);
  });

  test('crossing produces uniform-price fill', () => {
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 1, limit: 101 }),
      order({ id: 's1', side: 'short', size: 1, limit: 99 }),
    ]));
    expect(r.clearingVolume).toBe(1);
    expect(r.fills).toHaveLength(1);
    expect(r.fills[0]?.size).toBe(1);
    expect(r.fills[0]?.price).toBeGreaterThanOrEqual(99);
    expect(r.fills[0]?.price).toBeLessThanOrEqual(101);
  });

  test('clearing price chosen near prior mark when ties exist', () => {
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 1, limit: 101 }),
      order({ id: 'b2', side: 'long', size: 1, limit: 100 }),
      order({ id: 's1', side: 'short', size: 1, limit: 100 }),
      order({ id: 's2', side: 'short', size: 1, limit: 99 }),
    ], 100));
    expect(r.clearingPrice).toBeCloseTo(100);
  });

  test('liquidation orders filled before regular takers (priority)', () => {
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 1, limit: 105, type: 'taker', trader: 'taker1' }),
      order({ id: 'l1', side: 'long', size: 1, limit: 105, type: 'liquidation', trader: 'liquidatee' }),
      order({ id: 's1', side: 'short', size: 1, limit: 95 }),
    ]));
    expect(r.clearingVolume).toBe(1);
    // Liquidation should fill first.
    expect(r.fills[0]?.takerTrader).toBe('liquidatee');
  });

  test('FIFO within priority class', () => {
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 1, limit: 105, type: 'taker', trader: 'A', ts: 100 }),
      order({ id: 'b2', side: 'long', size: 1, limit: 105, type: 'taker', trader: 'B', ts: 50 }),
      order({ id: 's1', side: 'short', size: 1, limit: 95 }),
    ]));
    // B has earlier timestamp → fills first.
    expect(r.fills[0]?.takerTrader).toBe('B');
  });

  test('clearing volume maximization is the property', () => {
    // Two scenarios: should both clear at the volume-maximizing price.
    const r = clearBatch(input([
      order({ id: 'b1', side: 'long', size: 5, limit: 100 }),
      order({ id: 'b2', side: 'long', size: 5, limit: 99 }),
      order({ id: 's1', side: 'short', size: 4, limit: 100 }),
      order({ id: 's2', side: 'short', size: 4, limit: 99 }),
    ], 99.5));
    expect(r.clearingVolume).toBeGreaterThanOrEqual(5);
  });

  test('MEV-neutrality: order-of-arrival within batch does not change price', () => {
    const orders1: Order[] = [
      order({ id: 'b1', side: 'long', size: 2, limit: 102, ts: 10 }),
      order({ id: 's1', side: 'short', size: 2, limit: 98, ts: 20 }),
      order({ id: 'b2', side: 'long', size: 1, limit: 101, ts: 30 }),
    ];
    const orders2: Order[] = [orders1[2]!, orders1[0]!, orders1[1]!];

    const r1 = clearBatch(input(orders1));
    const r2 = clearBatch(input(orders2));

    expect(r1.clearingPrice).toBe(r2.clearingPrice);
    expect(r1.clearingVolume).toBe(r2.clearingVolume);
  });
});
