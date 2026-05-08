// MarketParams shape mirrors programs/flash-book/src/state.rs MarketParams.
// All fields are u32/u64/u8 — represented as plain `number` here; callers
// must pass via Anchor BN where required. This file only contains the
// canonical types and a sensible default for major perps markets.

import BN from 'bn.js';

export interface MarketParamsRaw {
  tickSize: BN;
  baseLotSize: BN;
  quoteLotSize: BN;
  minBaseLots: BN;

  takerFeeBps: number;
  makerRebateBps: number;
  toxicityTaxMaxBps: number;

  liqPenaltyBps: number;
  maintenanceMarginRatioBps: number;
  initialMarginRatioBps: number;
  maxLeverage: number;

  fundingRateMaxBpsPerSec: number;
  fundingRateKBps: number;

  oracleBandBps: number;

  flpSpreadBaseBps: number;
  flpSpreadAlphaBps: number;
  flpSpreadBetaBps: number;
  flpSpreadGammaBps: number;
  flpSpreadKappaBps: number;
  flpSpreadDeltaBps: number;
  flpInventoryLambdaBps: number;
  flpDepthFloorLots: BN;
  flpMaxGrowthPerBatchBps: number;
  flpQuoteLevels: number;

  vpinBucketSizeLots: BN;
  vpinEmaWindow: number;

  twapWindow: number;
  batchIntervalMs: number;
}

/** Sensible default parameter set for SOL/BTC/ETH-style major markets. */
export function defaultMajorMarketParams(): MarketParamsRaw {
  return {
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
  };
}

/**
 * Insurance fund initialization defaults. Three contribution streams,
 * pause threshold sized to ~5K quote-lots (governance-tunable).
 */
export interface InsuranceFundInitParams {
  feeContributionBps: number;
  toxicityTaxContributionBps: number;
  liqPenaltyContributionBps: number;
  pauseThresholdQuoteLots: BN;
}

export function defaultInsuranceFundParams(): InsuranceFundInitParams {
  return {
    feeContributionBps: 1_000,    // 10% of fees
    toxicityTaxContributionBps: 5_000,
    liqPenaltyContributionBps: 5_000,
    pauseThresholdQuoteLots: new BN(5_000),
  };
}
