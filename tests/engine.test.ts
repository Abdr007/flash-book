import { describe, expect, test } from 'bun:test';
import { DEFAULT_MAJOR_MARKET_PARAMS, FlashBookEngine } from '../src/index.ts';
import { buildCommitHash } from '../src/commit-reveal.ts';
import type { EngineConfig } from '../src/types.ts';

function makeConfig(): EngineConfig {
  return {
    scenarios: [],
    insuranceFund: {
      initialBalance: 100_000,
      feeContributionRate: 0.1,
      toxicityTaxContributionRate: 0.5,
      liqPenaltyContributionRate: 0.5,
      pauseNewPositionsBelow: 1_000,
    },
    commitRevealEnabled: true,
    commitExpiryBatches: 5,
    commitBondLamports: 1000,
  };
}

function makeEngine() {
  const e = new FlashBookEngine(makeConfig());
  e.addMarket({
    symbol: 'SOL',
    initialOraclePrice: 100,
    initialFlpCapital: 1_000_000,
    params: DEFAULT_MAJOR_MARKET_PARAMS,
  });
  return e;
}

describe('FlashBookEngine', () => {
  test('add market and check state', () => {
    const e = makeEngine();
    expect(e.marketState('SOL').oraclePrice).toBe(100);
    expect(e.marketState('SOL').markPrice).toBe(100);
  });

  test('deposit + withdraw', () => {
    const e = makeEngine();
    e.deposit('alice', 1000);
    expect(e.collateral('alice')).toBe(1000);
    expect(e.withdraw('alice', 500)).toBe(true);
    expect(e.collateral('alice')).toBe(500);
    expect(e.withdraw('alice', 999)).toBe(false);
  });

  test('runBatch with no orders only fills FLP virtual quotes against itself = no fills', () => {
    const e = makeEngine();
    const r = e.runBatch(1000);
    const m = r.perMarket.get('SOL');
    expect(m).toBeDefined();
    // FLP bids and asks at oracle ± spread don't cross — no fills.
    expect(m!.clearingVolume).toBe(0);
    expect(r.invariantsHeld).toBe(true);
  });

  test('taker buy gets filled by FLP virtual ask', () => {
    const e = makeEngine();
    e.deposit('alice', 5_000);
    e.submitTakerOrder({
      trader: 'alice',
      market: 'SOL',
      side: 'long',
      size: 1,
      limitPrice: 105, // willing to pay up
    });
    const r = e.runBatch(1000);
    const m = r.perMarket.get('SOL');
    expect(m).toBeDefined();
    expect(m!.fills.length).toBeGreaterThan(0);
    const aliceFill = m!.fills.find((f) => f.takerTrader === 'alice');
    expect(aliceFill).toBeDefined();
    expect(aliceFill!.makerTrader).toBe('FLP_POOL');
    expect(r.invariantsHeld).toBe(true);
    // Alice now holds a long position.
    const positions = e.positionsOf('alice');
    expect(positions.length).toBe(1);
    expect(positions[0]?.side).toBe('long');
  });

  test('mm limit order fills inside FLP spread', () => {
    const e = makeEngine();
    e.deposit('mm', 10_000);
    e.deposit('alice', 5_000);
    // MM places ask at 100.02 (tighter than FLP).
    e.submitLimitOrder({
      trader: 'mm',
      market: 'SOL',
      side: 'short',
      size: 1,
      limitPrice: 100.02,
    });
    e.submitTakerOrder({
      trader: 'alice',
      market: 'SOL',
      side: 'long',
      size: 1,
      limitPrice: 105,
    });
    const r = e.runBatch(1000);
    const m = r.perMarket.get('SOL');
    expect(m).toBeDefined();
    const aliceFill = m!.fills.find((f) => f.takerTrader === 'alice');
    expect(aliceFill).toBeDefined();
    // Should fill against MM, not FLP, because MM is tighter.
    expect(aliceFill!.makerTrader).toBe('mm');
  });

  test('commit-reveal end-to-end', () => {
    const e = makeEngine();
    e.deposit('bob', 5_000);
    const payload = {
      market: 'SOL',
      trader: 'bob',
      side: 'long' as const,
      size: 1,
      limitPrice: 105,
      nonce: 'n42',
    };
    const hash = buildCommitHash(payload);
    e.submitCommit({ trader: 'bob', market: 'SOL', hash, bondLamports: 1000 });
    expect(e.pendingCommitsCount()).toBe(1);
    e.runBatch(1000); // batch 1 — commit registered
    const ok = e.submitReveal(payload);
    expect(ok).toBe(true);
    expect(e.pendingCommitsCount()).toBe(0);
    const r = e.runBatch(1050); // batch 2 — reveal matched
    const m = r.perMarket.get('SOL');
    expect(m!.fills.find((f) => f.takerTrader === 'bob')).toBeDefined();
  });

  test('mark price stays within oracle band', () => {
    const e = makeEngine();
    e.deposit('alice', 10_000);
    for (let i = 0; i < 10; i++) {
      e.submitTakerOrder({ trader: 'alice', market: 'SOL', side: 'long', size: 0.1, limitPrice: 110 });
      e.runBatch(1000 + i * 50);
    }
    const m = e.marketState('SOL');
    // Oracle band is 100bps; mark must be within ±1% of oracle.
    expect(m.markPrice).toBeGreaterThan(99);
    expect(m.markPrice).toBeLessThan(101);
  });

  test('initial margin enforced on taker submission', () => {
    const e = makeEngine();
    e.deposit('alice', 10);
    expect(() =>
      e.submitTakerOrder({
        trader: 'alice',
        market: 'SOL',
        side: 'long',
        size: 100,
        limitPrice: 105,
      }),
    ).toThrow();
  });

  test('invariants hold across many random batches', () => {
    const e = makeEngine();
    for (let i = 0; i < 5; i++) {
      e.deposit(`trader${i}`, 5000);
    }
    for (let b = 0; b < 50; b++) {
      // Random orders.
      for (let i = 0; i < 5; i++) {
        const t = `trader${i}`;
        if (Math.random() < 0.3) {
          try {
            e.submitTakerOrder({
              trader: t,
              market: 'SOL',
              side: Math.random() < 0.5 ? 'long' : 'short',
              size: 0.1,
              limitPrice: 100 * (1 + (Math.random() - 0.5) * 0.05),
            });
          } catch (_) { /* margin fail OK */ }
        }
      }
      const r = e.runBatch(1000 + b * 50);
      expect(r.invariantsHeld).toBe(true);
    }
  });
});
