import { describe, expect, test } from 'bun:test';
import { PublicKey } from '@solana/web3.js';
import { Backtester, type FillEvent } from '../src/backtester.ts';

const MARKET = new PublicKey('11111111111111111111111111111112');
const TRADER = new PublicKey('11111111111111111111111111111113');
const KEY = MARKET.toBase58();

const baseQuote = {
  baseSpreadBps: 10,
  vpinSpreadAlpha: 0.5,
  inventorySkewBpsPerUnit: 100,
  oiImbalanceSpreadCoef: 0,
  quoteSizeLots: 1n,
};

const baseLimits = {
  maxInventoryLots: 100n,
  maxDrawdownQuoteLots: -10_000_000n,
  minCollateralQuoteLots: 1_000n,
};

function fill(ts: number, priceTicks: bigint, sizeLots: bigint, takerSide: 'long' | 'short'): FillEvent {
  return { ts, market: MARKET, priceTicks, sizeLots, takerSide };
}

describe('Backtester', () => {
  test('runs to completion on an empty tape (no fills, just iterations)', () => {
    const bt = new Backtester({
      trader: TRADER,
      markets: [{ market: MARKET, quoteParams: baseQuote, priceDiffBps: 0 }],
      globalRiskLimits: baseLimits,
      initialCollateralQuoteLots: 1_000_000n,
      makerRebateBps: 1,
      tapes: new Map([
        [
          KEY,
          {
            market: MARKET,
            fills: [],
            initialMarkTicks: 100_000n,
            tickSize: 1n,
            minBaseLots: 1n,
          },
        ],
      ]),
      refreshMs: 100,
      maxIterations: 5,
    });
    const result = bt.run();
    expect(result.fills).toBe(0);
    expect(result.iterations).toBe(5);
  });

  test('absorbs maker fills when our quote crosses the trade', () => {
    // Tape: a single sell-taker trade at 99_950 ticks.
    // Bot quotes around mark 100_000 with 10 bps half-spread →
    // bid ≈ 99_900, ask ≈ 100_100. Trade at 99_950 with takerSide=short
    // (taker selling) hits our bid (we buy).
    const bt = new Backtester({
      trader: TRADER,
      markets: [{ market: MARKET, quoteParams: baseQuote, priceDiffBps: 0 }],
      globalRiskLimits: baseLimits,
      initialCollateralQuoteLots: 1_000_000n,
      makerRebateBps: 5, // higher rebate for visible PnL
      tapes: new Map([
        [
          KEY,
          {
            market: MARKET,
            fills: [fill(150, 99_900n, 1n, 'short')],
            initialMarkTicks: 100_000n,
            tickSize: 1n,
            minBaseLots: 1n,
          },
        ],
      ]),
      refreshMs: 100,
      maxIterations: 10,
    });
    const result = bt.run();
    expect(result.fills).toBe(1);
    // Net inventory should be +1 (we bought).
    expect(result.netInventoryByMarket.get(KEY)).toBe(1n);
    // Collateral grew by maker rebate.
    expect(result.finalCollateralQuoteLots).toBeGreaterThan(1_000_000n);
  });

  test('does not fill when quotes do not cross', () => {
    // Trade at 100_500 (taker buying high) — far above our ask (~100_100).
    // Wait, taker=long buying at 100_500 means they crossed up. Our ask
    // at 100_100 ≤ 100_500 → we ARE filled.
    // To test "does not fill", set the trade BELOW our quote (taker=long
    // at 99_500 — but takers don't usually buy at 99_500 against our ask
    // 100_100; the matching condition fails).
    const bt = new Backtester({
      trader: TRADER,
      markets: [{ market: MARKET, quoteParams: baseQuote, priceDiffBps: 0 }],
      globalRiskLimits: baseLimits,
      initialCollateralQuoteLots: 1_000_000n,
      makerRebateBps: 1,
      tapes: new Map([
        [
          KEY,
          {
            market: MARKET,
            fills: [fill(150, 99_500n, 1n, 'long')], // long taker at 99_500: ask 100_100 > 99_500 → no fill
            initialMarkTicks: 100_000n,
            tickSize: 1n,
            minBaseLots: 1n,
          },
        ],
      ]),
      refreshMs: 100,
      maxIterations: 5,
    });
    const result = bt.run();
    expect(result.fills).toBe(0);
    expect(result.netInventoryByMarket.get(KEY) ?? 0n).toBe(0n);
  });
});
