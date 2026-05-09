import { describe, expect, test } from 'bun:test';
import {
  computeQuote,
  checkRiskGates,
  type MarketSnapshot,
  type QuoteParams,
  type RiskLimits,
} from '../src/market-maker.ts';

const baseMarket: MarketSnapshot = {
  markPriceTicks: 100_000n,
  vpinBps: 0,
  tickSize: 1n,
  minBaseLots: 1n,
  oiImbalanceLots: 0n,
  oiTotalLots: 1_000n,
  currentBatch: 1n,
};

const baseParams: QuoteParams = {
  baseSpreadBps: 10, // 10 bps half-spread (1bps = 0.01%)
  vpinSpreadAlpha: 0.5,
  inventorySkewBpsPerUnit: 100, // 100 bps skew per 100% inventory
  oiImbalanceSpreadCoef: 0.1,
  quoteSizeLots: 5n,
};

describe('computeQuote', () => {
  test('flat inventory + no toxicity gives symmetric quotes around mark', () => {
    const q = computeQuote({
      market: baseMarket,
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
    });
    expect(q.empty).toBe(false);
    // Fair = mark when inventory = 0.
    expect(q.fairValueTicks).toBe(100_000n);
    // Symmetric: bid below mark, ask above, equidistant within tick rounding.
    expect(q.bidTicks).toBeLessThan(100_000n);
    expect(q.askTicks).toBeGreaterThan(100_000n);
    const bidGap = 100_000n - q.bidTicks;
    const askGap = q.askTicks - 100_000n;
    // Tolerate a 1-tick rounding difference between sides (floor vs ceil).
    expect(Number(bidGap - askGap >= -1n && bidGap - askGap <= 1n)).toBe(1);
  });

  test('long inventory skews fair down (more attractive ask)', () => {
    // 50_000 lots × 100_000 ticks × 1 tick_size = 5_000_000_000 notional.
    // capital = 100_000_000_000 → 5% inventory fraction.
    // skew = -100 × 0.05 = -5 bps (fair drops).
    const q = computeQuote({
      market: baseMarket,
      inventorySignedLots: 50_000n,
      capitalQuoteLots: 100_000_000_000n,
      params: baseParams,
    });
    expect(q.fairValueTicks).toBeLessThan(100_000n);
  });

  test('short inventory skews fair up', () => {
    const q = computeQuote({
      market: baseMarket,
      inventorySignedLots: -50_000n,
      capitalQuoteLots: 100_000_000_000n,
      params: baseParams,
    });
    expect(q.fairValueTicks).toBeGreaterThan(100_000n);
  });

  test('high VPIN widens spread', () => {
    const calm = computeQuote({
      market: { ...baseMarket, vpinBps: 0 },
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
    });
    const toxic = computeQuote({
      market: { ...baseMarket, vpinBps: 5_000 }, // 50% toxic
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
    });
    expect(toxic.effectiveSpreadBps).toBeGreaterThan(calm.effectiveSpreadBps);
    expect(toxic.askTicks - toxic.bidTicks).toBeGreaterThan(calm.askTicks - calm.bidTicks);
  });

  test('skipBid sets bidTicks to 0 (only ask)', () => {
    const q = computeQuote({
      market: baseMarket,
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
      skipBid: true,
    });
    expect(q.empty).toBe(false);
    expect(q.bidTicks).toBe(0n);
    expect(q.askTicks).toBeGreaterThan(0n);
  });

  test('skipBid + skipAsk → empty', () => {
    const q = computeQuote({
      market: baseMarket,
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
      skipBid: true,
      skipAsk: true,
    });
    expect(q.empty).toBe(true);
  });

  test('zero or negative mark → empty', () => {
    const q = computeQuote({
      market: { ...baseMarket, markPriceTicks: 0n },
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
    });
    expect(q.empty).toBe(true);
  });

  test('quote ticks are aligned to tick_size', () => {
    const q = computeQuote({
      market: { ...baseMarket, tickSize: 10n },
      inventorySignedLots: 0n,
      capitalQuoteLots: 1_000_000n,
      params: baseParams,
    });
    expect(q.bidTicks % 10n).toBe(0n);
    expect(q.askTicks % 10n).toBe(0n);
  });
});

const baseLimits: RiskLimits = {
  maxInventoryLots: 100n,
  maxDrawdownQuoteLots: -10_000n,
  minCollateralQuoteLots: 1_000n,
};

describe('checkRiskGates', () => {
  test('healthy state allows both sides', () => {
    const r = checkRiskGates({
      inventorySignedLots: 0n,
      collateralQuoteLots: 100_000n,
      realizedPnlQuoteLots: 0n,
      limits: baseLimits,
      quoteSizeLots: 5n,
    });
    expect(r.canQuote).toBe(true);
    expect(r.skipBid).toBe(false);
    expect(r.skipAsk).toBe(false);
    expect(r.killSwitchActive).toBe(false);
  });

  test('drawdown breach trips kill switch', () => {
    const r = checkRiskGates({
      inventorySignedLots: 0n,
      collateralQuoteLots: 100_000n,
      realizedPnlQuoteLots: -20_000n,
      limits: baseLimits,
      quoteSizeLots: 5n,
    });
    expect(r.killSwitchActive).toBe(true);
    expect(r.canQuote).toBe(false);
  });

  test('low collateral blocks quoting (no kill switch)', () => {
    const r = checkRiskGates({
      inventorySignedLots: 0n,
      collateralQuoteLots: 500n,
      realizedPnlQuoteLots: 0n,
      limits: baseLimits,
      quoteSizeLots: 5n,
    });
    expect(r.canQuote).toBe(false);
    expect(r.killSwitchActive).toBe(false);
  });

  test('long inventory at cap blocks bid only', () => {
    const r = checkRiskGates({
      inventorySignedLots: 100n, // already at cap
      collateralQuoteLots: 100_000n,
      realizedPnlQuoteLots: 0n,
      limits: baseLimits,
      quoteSizeLots: 5n,
    });
    expect(r.skipBid).toBe(true);
    expect(r.skipAsk).toBe(false);
    expect(r.canQuote).toBe(true);
  });

  test('short inventory at cap blocks ask only', () => {
    const r = checkRiskGates({
      inventorySignedLots: -100n,
      collateralQuoteLots: 100_000n,
      realizedPnlQuoteLots: 0n,
      limits: baseLimits,
      quoteSizeLots: 5n,
    });
    expect(r.skipBid).toBe(false);
    expect(r.skipAsk).toBe(true);
  });
});
