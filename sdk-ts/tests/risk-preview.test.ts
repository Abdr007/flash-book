import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  defaultScenarios,
  initialMarginRequired,
  previewPortfolioRisk,
  type StressScenario,
} from '../src/risk-preview.ts';
import type { MarketAccount, PositionAccount } from '../src/accounts.ts';

const MARKET_PK = Keypair.generate().publicKey;

function mockMarket(markPrice = 100_000): MarketAccount {
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
    currentBatch: new BN(0),
    lastBatchMs: new BN(0),
    oraclePriceTicks: new BN(markPrice),
    oracleConfidence: new BN(0),
    markPriceTicks: new BN(markPrice),
    cumFundingIndex: new BN(0),
    lastFundingRateBpsPerSec: new BN(0),
    vpin: {
      buyPending: new BN(0),
      sellPending: new BN(0),
      bucketsObserved: new BN(0),
      valueQ32_32: new BN(0),
    },
    oiLongLots: new BN(0),
    oiShortLots: new BN(0),
    recentClearingPrices: [],
    recentClearingCount: 0,
    totalFeesCollected: new BN(0),
    totalToxicityTaxCollected: new BN(0),
    totalLiquidations: new BN(0),
    params: {
      tickSize: new BN(1),
      baseLotSize: new BN(1_000),
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
  };
}

function mockPosition(side: number, sizeLots: number, entry: number): PositionAccount {
  return {
    trader: Keypair.generate().publicKey,
    market: MARKET_PK,
    bump: 0,
    side,
    sizeLots: new BN(sizeLots),
    entryPriceTicks: new BN(entry),
    collateralQuoteLots: new BN(0),
    cumFundingIndexAtEntry: new BN(0),
    realizedPnlQuoteLots: new BN(0),
    fundingPaidQuoteLots: new BN(0),
    lastSettlementBatch: new BN(0),
  };
}

describe('defaultScenarios', () => {
  test('includes flat + 8 per-market shocks + 4 correlated', () => {
    const scenarios = defaultScenarios([MARKET_PK.toBase58()]);
    expect(scenarios.length).toBe(1 + 8 + 4); // 13
    expect(scenarios[0]?.name).toBe('flat');
    expect(scenarios.some((s) => s.name === 'all_down_10pct')).toBe(true);
    expect(scenarios.some((s) => s.name === 'all_up_10pct')).toBe(true);
    expect(scenarios.some((s) => s.name === 'black_swan_down')).toBe(true);
    expect(scenarios.some((s) => s.name === 'black_swan_up')).toBe(true);
  });

  test('two markets generate 1 + 16 + 4 scenarios', () => {
    const m1 = Keypair.generate().publicKey.toBase58();
    const m2 = Keypair.generate().publicKey.toBase58();
    expect(defaultScenarios([m1, m2]).length).toBe(1 + 16 + 4);
  });

  test('shocks are signed bps integers', () => {
    const s = defaultScenarios([MARKET_PK.toBase58()]);
    const downBig = s.find((sc) => sc.name === 'black_swan_down');
    expect(downBig?.shocks.get(MARKET_PK.toBase58())).toBe(-3000);
  });
});

describe('previewPortfolioRisk', () => {
  test('flat empty portfolio is healthy', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const r = previewPortfolioRisk([], markets, 1_000);
    expect(r.isHealthy).toBe(true);
    expect(r.required).toBe(0);
    expect(r.equity).toBe(1_000);
  });

  test('long position with collateral 1% of notional is liquidatable', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const positions = [mockPosition(0, 100, 100_000)]; // 100 lots * 100_000 = 10M notional
    const r = previewPortfolioRisk(positions, markets, 100_000); // 1% collateral
    expect(r.isHealthy).toBe(false);
    expect(r.required).toBeGreaterThan(0);
  });

  test('long position with adequate collateral is healthy', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const positions = [mockPosition(0, 1, 100_000)]; // 1 lot * 100_000 = 100K notional
    const r = previewPortfolioRisk(positions, markets, 100_000); // 100% collateral
    expect(r.isHealthy).toBe(true);
    expect(r.healthRatio).toBeGreaterThan(1);
  });

  test('hedge collapses required margin (long+short same market)', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const unhedged = previewPortfolioRisk(
      [mockPosition(0, 100, 100_000)],
      markets,
      0,
    );
    const hedged = previewPortfolioRisk(
      [mockPosition(0, 100, 100_000), mockPosition(1, 100, 100_000)],
      markets,
      0,
    );
    expect(hedged.required).toBeLessThan(unhedged.required / 5);
  });

  test('worstScenario name reflects the binding constraint', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const positions = [mockPosition(0, 100, 100_000)]; // long
    const r = previewPortfolioRisk(positions, markets, 100);
    // Long is hurt by black-swan-down or large negative single-asset shocks.
    expect(r.worstScenario).not.toBe('flat');
  });

  test('zero-size positions are skipped', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const positions = [mockPosition(0, 0, 100_000)]; // empty
    const r = previewPortfolioRisk(positions, markets, 100_000);
    expect(r.isHealthy).toBe(true);
    expect(r.required).toBe(0);
  });

  test('healthRatio is +Infinity for zero-required portfolio', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const r = previewPortfolioRisk([], markets, 0);
    expect(r.healthRatio).toBe(Number.POSITIVE_INFINITY);
  });

  test('caller-supplied scenarios are respected', () => {
    const markets = new Map<string, MarketAccount>([[MARKET_PK.toBase58(), mockMarket()]]);
    const positions = [mockPosition(0, 100, 100_000)];
    const flatOnly: StressScenario[] = [{ name: 'flat', shocks: new Map() }];
    const r = previewPortfolioRisk(positions, markets, 0, flatOnly);
    // Flat scenario only — required = maintenance margin on flat notional.
    expect(r.worstScenario).toBe('flat');
    expect(r.required).toBeGreaterThan(0);
  });
});

describe('initialMarginRequired', () => {
  test('matches the formula: size × price × tick × ratio', () => {
    const market = mockMarket();
    const im = initialMarginRequired(10, 100_000, market);
    // 10 * 100_000 * 1 * 0.025 = 25_000
    expect(im).toBe(25_000);
  });
});
