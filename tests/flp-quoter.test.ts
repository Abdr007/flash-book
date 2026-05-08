import { describe, expect, test } from 'bun:test';
import { generateFlpQuotes } from '../src/flp-quoter.ts';
import { makeTestMarket } from './_helpers.ts';

describe('FLP Quoter', () => {
  test('emits balanced ladder when pool is flat', () => {
    const m = makeTestMarket('SOL', 100);
    const out = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    expect(out.skew).toBeCloseTo(0);
    expect(out.fairValue).toBeCloseTo(100);
    expect(out.bidLadder.length).toBe(m.params.flpQuoteLevels);
    expect(out.askLadder.length).toBe(m.params.flpQuoteLevels);
    // First bid below fair, first ask above fair.
    expect(out.bidLadder[0]!.price).toBeLessThan(100);
    expect(out.askLadder[0]!.price).toBeGreaterThan(100);
  });

  test('inventory skew: pool net-short → fair value above oracle', () => {
    const m = makeTestMarket('SOL', 100);
    const out = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: -100_000, // pool is short $100k
      poolGrossUtilization: 0.1,
      nowMs: 0,
      batchNum: 1,
    });
    expect(out.skew).toBeGreaterThan(0);
    expect(out.fairValue).toBeGreaterThan(100);
  });

  test('inventory skew: pool net-long → fair value below oracle', () => {
    const m = makeTestMarket('SOL', 100);
    const out = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 100_000,
      poolGrossUtilization: 0.1,
      nowMs: 0,
      batchNum: 1,
    });
    expect(out.skew).toBeLessThan(0);
    expect(out.fairValue).toBeLessThan(100);
  });

  test('VPIN spike widens spread', () => {
    const calm = makeTestMarket('SOL', 100);
    const toxic = makeTestMarket('SOL', 100);
    toxic.vpin = 1.0;
    const calmOut = generateFlpQuotes({
      market: calm,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    const toxicOut = generateFlpQuotes({
      market: toxic,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    expect(toxicOut.effectiveSpread).toBeGreaterThan(calmOut.effectiveSpread);
  });

  test('utilization widens spread', () => {
    const m = makeTestMarket('SOL', 100);
    const lowU = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0.0,
      nowMs: 0,
      batchNum: 1,
    });
    const highU = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0.8,
      nowMs: 0,
      batchNum: 1,
    });
    expect(highU.effectiveSpread).toBeGreaterThan(lowU.effectiveSpread);
  });

  test('pool with zero capital emits no quotes', () => {
    const m = makeTestMarket('SOL', 100);
    const out = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 0,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    expect(out.orders).toHaveLength(0);
  });

  test('per-batch growth cap respected', () => {
    const m = makeTestMarket('SOL', 100);
    const out = generateFlpQuotes({
      market: m,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    // Each side: total quoted USD ≤ pool * flpMaxGrowthPerBatchPct
    let totalBidUsd = 0;
    for (const lvl of out.bidLadder) totalBidUsd += lvl.size * lvl.price;
    expect(totalBidUsd).toBeLessThanOrEqual(1_000_000 * m.params.flpMaxGrowthPerBatchPct + 1);
  });

  test('realized vol from recent prices widens spread', () => {
    const calm = makeTestMarket('SOL', 100);
    calm.recentClearingPrices = [100, 100.01, 100, 99.99, 100];
    const volatile = makeTestMarket('SOL', 100);
    volatile.recentClearingPrices = [100, 105, 95, 102, 98];
    const calmOut = generateFlpQuotes({
      market: calm,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    const volOut = generateFlpQuotes({
      market: volatile,
      poolCapitalUsd: 1_000_000,
      poolNetUsd: 0,
      poolGrossUtilization: 0,
      nowMs: 0,
      batchNum: 1,
    });
    expect(volOut.realizedVol).toBeGreaterThan(calmOut.realizedVol);
    expect(volOut.effectiveSpread).toBeGreaterThan(calmOut.effectiveSpread);
  });
});
