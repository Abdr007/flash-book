import { describe, expect, test } from 'bun:test';
import { advanceFundingIndex, fundingOwed, settleFunding } from '../src/funding.ts';
import type { MarketState, Position } from '../src/types.ts';
import { makeTestMarket } from './_helpers.ts';

function makeMarket(oracle: number, mark: number): MarketState {
  const m = makeTestMarket('SOL', oracle);
  m.markPrice = mark;
  return m;
}

describe('funding', () => {
  test('zero blockDelta does nothing', () => {
    const m = makeMarket(100, 101);
    const t = advanceFundingIndex(m, 0);
    expect(t.indexDelta).toBe(0);
  });

  test('positive premium → positive rate (longs pay)', () => {
    const m = makeMarket(100, 101); // 1% premium
    const t = advanceFundingIndex(m, 1000); // 1 sec
    expect(t.rate).toBeGreaterThan(0);
    expect(t.indexDelta).toBeGreaterThan(0);
  });

  test('negative premium → negative rate (shorts pay)', () => {
    const m = makeMarket(100, 99);
    const t = advanceFundingIndex(m, 1000);
    expect(t.rate).toBeLessThan(0);
    expect(t.indexDelta).toBeLessThan(0);
  });

  test('rate clamped by max', () => {
    const m = makeMarket(100, 1000); // huge premium
    const t = advanceFundingIndex(m, 1000);
    expect(Math.abs(t.rate)).toBeLessThanOrEqual(m.params.fundingRateMaxPerSec + 1e-12);
  });

  test('long position pays when index increases', () => {
    const m = makeMarket(100, 101);
    advanceFundingIndex(m, 60_000); // 1 min
    const pos: Position = {
      trader: 'A',
      market: 'SOL',
      side: 'long',
      size: 1,
      entryPrice: 100,
      collateral: 100,
      cumFundingIndexAtEntry: 0,
      realizedPnl: 0,
      fundingPaid: 0,
    };
    const owed = fundingOwed(pos, m);
    expect(owed).toBeGreaterThan(0);
  });

  test('settleFunding deducts from collateral and resets index', () => {
    const m = makeMarket(100, 101);
    advanceFundingIndex(m, 60_000);
    const pos: Position = {
      trader: 'A',
      market: 'SOL',
      side: 'long',
      size: 1,
      entryPrice: 100,
      collateral: 100,
      cumFundingIndexAtEntry: 0,
      realizedPnl: 0,
      fundingPaid: 0,
    };
    const owed = settleFunding(pos, m);
    expect(pos.fundingPaid).toBe(owed);
    expect(pos.cumFundingIndexAtEntry).toBe(m.cumFundingIndex);
    expect(pos.collateral).toBeCloseTo(100 - owed);
  });
});
