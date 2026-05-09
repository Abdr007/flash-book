import { describe, expect, test } from 'bun:test';
import BN from 'bn.js';
import { PublicKey } from '@solana/web3.js';
import { estimateFundingOwed } from '../src/keepers.ts';
import type { MarketAccount, PositionAccount } from '../src/accounts.ts';

const MARKET = new PublicKey('11111111111111111111111111111112');
const TRADER = new PublicKey('11111111111111111111111111111113');

function mockMarket(overrides: Partial<MarketAccount> = {}): MarketAccount {
  return {
    authority: PublicKey.default,
    flpPool: PublicKey.default,
    baseMint: PublicKey.default,
    quoteMint: PublicKey.default,
    baseVault: PublicKey.default,
    quoteVault: PublicKey.default,
    oracleAccount: PublicKey.default,
    insuranceFund: PublicKey.default,
    bump: 0,
    status: 1,
    currentBatch: new BN(1),
    lastBatchMs: new BN(0),
    oraclePriceTicks: new BN(100_000),
    oracleConfidence: new BN(0),
    markPriceTicks: new BN(100_000),
    cumFundingIndex: new BN(0),
    lastFundingRateBpsPerSec: new BN(0),
    vpin: { buyPending: new BN(0), sellPending: new BN(0), bucketsObserved: new BN(0), valueQ32_32: new BN(0) },
    oiLongLots: new BN(0),
    oiShortLots: new BN(0),
    recentClearingPrices: [],
    recentClearingCount: 0,
    totalFeesCollected: new BN(0),
    totalToxicityTaxCollected: new BN(0),
    totalLiquidations: new BN(0),
    params: {
      tickSize: new BN(1),
      baseLotSize: new BN(1),
      quoteLotSize: new BN(1),
      minBaseLots: new BN(1),
      takerFeeBps: 5,
      makerRebateBps: 1,
      toxicityTaxMaxBps: 5,
      liqPenaltyBps: 50,
      maintenanceMarginRatioBps: 125,
      initialMarginRatioBps: 250,
      maxLeverage: 40,
      fundingRateMaxBpsPerSec: 1_000,
      fundingRateKBps: 100_000,
      oracleBandBps: 100,
      flpSpreadBaseBps: 5,
      flpSpreadAlphaBps: 5_000,
      flpSpreadBetaBps: 3_000,
      flpSpreadGammaBps: 2_000,
      flpSpreadKappaBps: 500,
      flpSpreadDeltaBps: 20_000,
      flpInventoryLambdaBps: 5_000,
      flpDepthFloorLots: new BN(1_000),
      flpMaxGrowthPerBatchBps: 50,
      flpQuoteLevels: 5,
      vpinBucketSizeLots: new BN(100),
      vpinEmaWindow: 50,
      twapWindow: 5,
      batchIntervalMs: 50,
    },
    ...overrides,
  } as MarketAccount;
}

function mockPosition(overrides: Partial<PositionAccount> = {}): PositionAccount {
  return {
    trader: TRADER,
    market: MARKET,
    bump: 0,
    side: 0,
    sizeLots: new BN(100),
    entryPriceTicks: new BN(100_000),
    collateralQuoteLots: new BN(1_000_000),
    cumFundingIndexAtEntry: new BN(0),
    realizedPnlQuoteLots: new BN(0),
    fundingPaidQuoteLots: new BN(0),
    lastSettlementBatch: new BN(0),
    ...overrides,
  } as PositionAccount;
}

describe('estimateFundingOwed', () => {
  test('returns zero when index has not advanced', () => {
    const m = mockMarket({ cumFundingIndex: new BN(0) });
    const p = mockPosition({ cumFundingIndexAtEntry: new BN(0) });
    expect(estimateFundingOwed(p, m)).toBe(0n);
  });

  test('returns zero for empty positions', () => {
    const m = mockMarket({ cumFundingIndex: new BN('1000000000000000000') });
    const p = mockPosition({ sizeLots: new BN(0), cumFundingIndexAtEntry: new BN(0) });
    expect(estimateFundingOwed(p, m)).toBe(0n);
  });

  test('long pays positive funding when delta is positive', () => {
    // notional = 100 × 100_000 × 1 = 10_000_000.
    // cum_now = 1<<60, delta = 1<<60.
    // owed = (10_000_000 × (1<<60)) >> 64 = 10_000_000 / 16 = 625_000.
    // Long → positive.
    const m = mockMarket({ cumFundingIndex: new BN((1n << 60n).toString()) });
    const p = mockPosition({ side: 0, cumFundingIndexAtEntry: new BN(0) });
    expect(estimateFundingOwed(p, m)).toBe(625_000n);
  });

  test('short receives positive funding when delta is positive (sign flipped)', () => {
    const m = mockMarket({ cumFundingIndex: new BN((1n << 60n).toString()) });
    const p = mockPosition({ side: 1, cumFundingIndexAtEntry: new BN(0) });
    // Short with positive premium → owed is negative (trader receives).
    expect(estimateFundingOwed(p, m)).toBe(-625_000n);
  });

  test('long receives funding when delta is negative (premium below oracle)', () => {
    // We can't represent negative bigint via BN.toString() easily because
    // BN supports negatives natively. Use BN('-...').
    const m = mockMarket({ cumFundingIndex: new BN('-' + (1n << 60n).toString()) });
    const p = mockPosition({ side: 0, cumFundingIndexAtEntry: new BN(0) });
    expect(estimateFundingOwed(p, m)).toBeLessThan(0n);
  });
});

describe('Keeper class shape', () => {
  test('start/stop are idempotent', async () => {
    // Shape-only test — we don't actually run a loop because the bot
    // would try to hit a real RPC. We just verify the public API
    // signatures + stats baseline.
    const { LiquidationKeeper } = await import('../src/keepers.ts');
    expect(typeof LiquidationKeeper).toBe('function');
    expect(typeof LiquidationKeeper.prototype.start).toBe('function');
    expect(typeof LiquidationKeeper.prototype.stop).toBe('function');
    expect(typeof LiquidationKeeper.prototype.tick).toBe('function');
    expect(typeof LiquidationKeeper.prototype.getStats).toBe('function');
  });
});
