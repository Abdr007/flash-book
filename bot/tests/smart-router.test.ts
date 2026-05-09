import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey, type TransactionInstruction } from '@solana/web3.js';
import { SmartRouter } from '../src/smart-router.ts';
import type { Venue, MarketSnapshot, TraderSnapshot, PositionSnapshot } from '../src/types.ts';

const MARKET = new PublicKey('11111111111111111111111111111112');
const TRADER = new PublicKey('11111111111111111111111111111113');

function dummyIx(): TransactionInstruction {
  return { programId: PublicKey.default, keys: [], data: Buffer.alloc(0) };
}

function makeMockVenue(name: string, opts: {
  market?: MarketSnapshot | null;
  trader?: TraderSnapshot | null;
  position?: PositionSnapshot | null;
  seqs?: bigint[];
  ixCount?: number;
}): Venue & { placeCalls: number; cancelCalls: number; cancelSeqs: bigint[] } {
  const placeCount = opts.ixCount ?? 1;
  return {
    name,
    placeCalls: 0,
    cancelCalls: 0,
    cancelSeqs: [] as bigint[],
    fetchMarket: async () => opts.market ?? null,
    fetchTrader: async () => opts.trader ?? null,
    fetchPosition: async () => opts.position ?? null,
    fetchOpenOrderSeqs: async () => opts.seqs ?? [],
    buildQuoteInstructions: async function () {
      this.placeCalls += 1;
      return Array.from({ length: placeCount }, () => dummyIx());
    },
    buildCancelInstructions: async function (args) {
      this.cancelCalls += 1;
      this.cancelSeqs.push(...args.seqs);
      return Array.from({ length: args.seqs.length }, () => dummyIx());
    },
    sendTx: async () => 'sig',
  } as Venue & { placeCalls: number; cancelCalls: number; cancelSeqs: bigint[] };
}

describe('SmartRouter', () => {
  test('errors with zero venues', () => {
    expect(() => new SmartRouter({ venues: [] })).toThrow();
  });

  test('default policy picks the first venue with a non-zero mark', async () => {
    const v0 = makeMockVenue('v0', { market: null });
    const v1 = makeMockVenue('v1', { market: makeSnap(100_000n) });
    const r = new SmartRouter({ venues: [v0, v1] });
    const snap = await r.fetchMarket(MARKET);
    expect(snap?.markPriceTicks).toBe(100_000n);
    expect(r.getLastChosenVenueIndex()).toBe(1);
  });

  test('aggregates trader collateral across venues', async () => {
    const v0 = makeMockVenue('v0', { trader: { collateralQuoteLots: 1_000n, realizedPnlQuoteLots: 0n, openPositions: 1 } });
    const v1 = makeMockVenue('v1', { trader: { collateralQuoteLots: 2_000n, realizedPnlQuoteLots: 100n, openPositions: 2 } });
    const r = new SmartRouter({ venues: [v0, v1] });
    const t = await r.fetchTrader(TRADER);
    expect(t!.collateralQuoteLots).toBe(3_000n);
    expect(t!.realizedPnlQuoteLots).toBe(100n);
    expect(t!.openPositions).toBe(3);
  });

  test('nets signed inventory across venues', async () => {
    const v0 = makeMockVenue('v0', { position: { signedSizeLots: 10n, entryPriceTicks: 100_000n } });
    const v1 = makeMockVenue('v1', { position: { signedSizeLots: -3n, entryPriceTicks: 100_500n } });
    const r = new SmartRouter({ venues: [v0, v1] });
    const p = await r.fetchPosition(MARKET, TRADER);
    expect(p!.signedSizeLots).toBe(7n); // 10 - 3
  });

  test('packs venue id into bit 60 of returned seqs', async () => {
    const v0 = makeMockVenue('v0', { seqs: [3n, 7n] });
    const v1 = makeMockVenue('v1', { seqs: [5n] });
    const r = new SmartRouter({ venues: [v0, v1] });
    const seqs = await r.fetchOpenOrderSeqs(MARKET, TRADER);
    expect(seqs.length).toBe(3);
    // v0 ids: original seqs 3 and 7 (bit 60 = 0)
    expect(seqs).toContain(3n);
    expect(seqs).toContain(7n);
    // v1 id: original seq 5 with bit 60 set
    expect(seqs).toContain(5n | (1n << 60n));
  });

  test('cancel routes seqs back to their original venues', async () => {
    const v0 = makeMockVenue('v0', {});
    const v1 = makeMockVenue('v1', {});
    const r = new SmartRouter({ venues: [v0, v1] });
    await r.buildCancelInstructions({
      trader: TRADER,
      market: MARKET,
      seqs: [3n, 5n | (1n << 60n)],
    });
    expect(v0.cancelSeqs).toEqual([3n]);
    expect(v1.cancelSeqs).toEqual([5n]);
  });
});

function makeSnap(mark: bigint): MarketSnapshot {
  return {
    markPriceTicks: mark,
    vpinBps: 0,
    tickSize: 1n,
    minBaseLots: 1n,
    oiImbalanceLots: 0n,
    oiTotalLots: 0n,
    currentBatch: 1n,
  };
}
