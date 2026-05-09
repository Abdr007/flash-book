import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import { Strategy } from '../src/strategy.ts';
import type { MarketSnapshot, TraderSnapshot } from '../src/types.ts';

const trader = Keypair.generate().publicKey;
const marketA = new PublicKey('11111111111111111111111111111112');
const marketB = new PublicKey('11111111111111111111111111111113');

function mkSnap(mark: bigint, vpinBps = 0): MarketSnapshot {
  return {
    markPriceTicks: mark,
    vpinBps,
    tickSize: 1n,
    minBaseLots: 1n,
    oiImbalanceLots: 0n,
    oiTotalLots: 1_000n,
    currentBatch: 1n,
  };
}

function mkTrader(collateral = 1_000_000n, pnl = 0n): TraderSnapshot {
  return { collateralQuoteLots: collateral, realizedPnlQuoteLots: pnl, openPositions: 0 };
}

const baseQuote = {
  baseSpreadBps: 10,
  vpinSpreadAlpha: 0.5,
  inventorySkewBpsPerUnit: 100,
  oiImbalanceSpreadCoef: 0.05,
  quoteSizeLots: 1n,
};

const baseLimits = {
  maxInventoryLots: 100n,
  maxDrawdownQuoteLots: -10_000n,
  minCollateralQuoteLots: 1_000n,
};

describe('Strategy', () => {
  test('emits a place action on first iteration with no live quote', () => {
    const s = new Strategy({
      trader,
      markets: [{ market: marketA, quoteParams: baseQuote }],
      globalRiskLimits: baseLimits,
    });
    const out = s.decide({
      trader: mkTrader(),
      markets: new Map([[marketA.toBase58(), mkSnap(100_000n)]]),
      positions: new Map(),
      openOrderSeqs: new Map(),
    });
    expect(out.actions.length).toBe(1);
    expect(out.actions[0]!.type).toBe('place');
  });

  test('emits a noop when prices are within diff window', () => {
    const s = new Strategy({
      trader,
      markets: [{ market: marketA, quoteParams: baseQuote, priceDiffBps: 100 }],
      globalRiskLimits: baseLimits,
    });
    // First call: place.
    s.decide({
      trader: mkTrader(),
      markets: new Map([[marketA.toBase58(), mkSnap(100_000n)]]),
      positions: new Map(),
      openOrderSeqs: new Map([[marketA.toBase58(), []]]),
    });
    // Second call with same snapshot: should noop because diff ≤ 100bps.
    const out = s.decide({
      trader: mkTrader(),
      markets: new Map([[marketA.toBase58(), mkSnap(100_000n)]]),
      positions: new Map(),
      openOrderSeqs: new Map([[marketA.toBase58(), [1n]]]),
    });
    expect(out.actions[0]!.type).toBe('noop');
  });

  test('emits an edit action when an open seq exists and prices move', () => {
    const s = new Strategy({
      trader,
      markets: [{ market: marketA, quoteParams: baseQuote, priceDiffBps: 0 }],
      globalRiskLimits: baseLimits,
    });
    // First call: place.
    s.decide({
      trader: mkTrader(),
      markets: new Map([[marketA.toBase58(), mkSnap(100_000n)]]),
      positions: new Map(),
      openOrderSeqs: new Map([[marketA.toBase58(), []]]),
    });
    // Second call: prices ≠ live, open seqs present → edit.
    const out = s.decide({
      trader: mkTrader(),
      markets: new Map([[marketA.toBase58(), mkSnap(100_500n)]]),
      positions: new Map(),
      openOrderSeqs: new Map([[marketA.toBase58(), [1n]]]),
    });
    expect(out.actions[0]!.type).toBe('edit');
  });

  test('drawdown kill switch cancels open orders + flips killSwitchActive', () => {
    const s = new Strategy({
      trader,
      markets: [{ market: marketA, quoteParams: baseQuote }],
      globalRiskLimits: baseLimits,
    });
    const out = s.decide({
      trader: mkTrader(1_000_000n, -20_000n), // pnl below drawdown floor
      markets: new Map([[marketA.toBase58(), mkSnap(100_000n)]]),
      positions: new Map(),
      openOrderSeqs: new Map([[marketA.toBase58(), [1n, 2n]]]),
    });
    expect(out.killSwitchActive).toBe(true);
    expect(out.actions[0]!.type).toBe('cancel');
  });

  test('multi-market: independent quoting + per-market noop isolation', () => {
    const s = new Strategy({
      trader,
      markets: [
        { market: marketA, quoteParams: baseQuote, priceDiffBps: 100 },
        { market: marketB, quoteParams: baseQuote, priceDiffBps: 0 },
      ],
      globalRiskLimits: baseLimits,
    });
    // First call: place on both.
    s.decide({
      trader: mkTrader(),
      markets: new Map([
        [marketA.toBase58(), mkSnap(100_000n)],
        [marketB.toBase58(), mkSnap(200_000n)],
      ]),
      positions: new Map(),
      openOrderSeqs: new Map([
        [marketA.toBase58(), []],
        [marketB.toBase58(), []],
      ]),
    });
    // Second call: A unchanged (within 100bps), B moves slightly (must re-quote
    // because priceDiffBps=0).
    const out = s.decide({
      trader: mkTrader(),
      markets: new Map([
        [marketA.toBase58(), mkSnap(100_000n)],
        [marketB.toBase58(), mkSnap(200_001n)],
      ]),
      positions: new Map(),
      openOrderSeqs: new Map([
        [marketA.toBase58(), [1n]],
        [marketB.toBase58(), [2n]],
      ]),
    });
    const aAction = out.actions.find((a) => a.market.equals(marketA))!;
    const bAction = out.actions.find((a) => a.market.equals(marketB))!;
    expect(aAction.type).toBe('noop');
    expect(bAction.type).toBe('edit');
  });

  test('snapshot returns aggregate stats across markets', () => {
    const s = new Strategy({
      trader,
      markets: [
        { market: marketA, quoteParams: baseQuote },
        { market: marketB, quoteParams: baseQuote },
      ],
      globalRiskLimits: baseLimits,
    });
    s.decide({
      trader: mkTrader(),
      markets: new Map([
        [marketA.toBase58(), mkSnap(100_000n)],
        [marketB.toBase58(), mkSnap(200_000n)],
      ]),
      positions: new Map(),
      openOrderSeqs: new Map(),
    });
    const snap = s.snapshot(false, 0n);
    expect(snap.perMarket.length).toBe(2);
    expect(snap.iterationsCompleted).toBe(1);
  });
});
