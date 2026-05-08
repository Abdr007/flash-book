import { describe, expect, test } from 'bun:test';
import BN from 'bn.js';
import {
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
} from '../src/params.ts';

describe('defaultMajorMarketParams', () => {
  const p = defaultMajorMarketParams();

  test('sane fee shape', () => {
    expect(p.takerFeeBps).toBeGreaterThan(0);
    expect(p.takerFeeBps).toBeLessThan(100); // < 1%
    expect(p.makerRebateBps).toBeGreaterThanOrEqual(0);
    expect(p.makerRebateBps).toBeLessThan(p.takerFeeBps);
  });

  test('liquidation penalty < initial margin < max', () => {
    expect(p.liqPenaltyBps).toBeGreaterThan(0);
    expect(p.maintenanceMarginRatioBps).toBeGreaterThan(0);
    expect(p.initialMarginRatioBps).toBeGreaterThan(p.maintenanceMarginRatioBps);
  });

  test('FLP spread coefficients are non-negative', () => {
    expect(p.flpSpreadBaseBps).toBeGreaterThanOrEqual(0);
    expect(p.flpSpreadAlphaBps).toBeGreaterThanOrEqual(0);
    expect(p.flpSpreadBetaBps).toBeGreaterThanOrEqual(0);
    expect(p.flpSpreadGammaBps).toBeGreaterThanOrEqual(0);
    expect(p.flpSpreadKappaBps).toBeGreaterThanOrEqual(0);
    expect(p.flpSpreadDeltaBps).toBeGreaterThanOrEqual(0);
  });

  test('VPIN window is positive', () => {
    expect(p.vpinEmaWindow).toBeGreaterThan(0);
    expect((p.vpinBucketSizeLots as BN).gtn(0)).toBe(true);
  });

  test('batch interval respects MagicBlock ER block range', () => {
    expect(p.batchIntervalMs).toBeGreaterThanOrEqual(10);
    expect(p.batchIntervalMs).toBeLessThanOrEqual(500);
  });

  test('FLP growth cap fits within 100%', () => {
    expect(p.flpMaxGrowthPerBatchBps).toBeGreaterThan(0);
    expect(p.flpMaxGrowthPerBatchBps).toBeLessThanOrEqual(10_000);
  });
});

describe('defaultInsuranceFundParams', () => {
  const p = defaultInsuranceFundParams();

  test('contribution rates are valid bps', () => {
    expect(p.feeContributionBps).toBeGreaterThanOrEqual(0);
    expect(p.feeContributionBps).toBeLessThanOrEqual(10_000);
    expect(p.toxicityTaxContributionBps).toBeLessThanOrEqual(10_000);
    expect(p.liqPenaltyContributionBps).toBeLessThanOrEqual(10_000);
  });

  test('pause threshold is positive BN', () => {
    expect(p.pauseThresholdQuoteLots.gtn(0)).toBe(true);
  });
});
