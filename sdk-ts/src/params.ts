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

  /** Max age (s) for an oracle price; 0 = unlimited. */
  oracleStalenessMaxSeconds: number;
  /** Max oracle confidence as fraction of price (bps); 0 = unlimited. */
  oracleConfidenceMaxBps: number;
  /** Per-trader max position size on this market (lots); 0 = unlimited. */
  maxPositionLotsPerTrader: BN;
  /** Multi-oracle quorum max dispersion (bps of median); 0 = no check. */
  oracleQuorumMaxDispersionBps: number;
  /** Per-trader max notional as bps of FLP capital; 0 = unlimited. */
  maxPositionRatioBps: number;

  /** Bps of liq penalty paid to the keeper that triggers it. 0 = off. */
  liquidatorRewardBps: number;
  /** Cooldown (slots) between consecutive liquidations on a position. */
  liquidationCooldownSlots: number;
  /** Slots over which the liquidator reward Dutch-grows from base→full. */
  liquidationAuctionDurationSlots: number;
  /** JIT bonus rebate (bps) for makers filling JIT-flagged taker orders. */
  jitBonusRebateBps: number;
  /** Bps of net fee credited to a taker's referrer (Hyperliquid affiliate). */
  referrerShareBps: number;
  /** Bps of net fee credited to a taker's approved builder (HL builder codes). */
  builderShareBps: number;
  /** Bps of net fee credited to a permissionless market's deployer (HIP-3). */
  creatorShareBps: number;
  /** Pre-launch market flag — Hyperliquid pre-TGE perp pattern. */
  isPreLaunch: boolean;
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

    // Oracle hardening — sized to current Pyth + cex-aggregator observed
    // staleness/confidence on Solana mainnet.
    oracleStalenessMaxSeconds: 30,
    oracleConfidenceMaxBps: 100, // 1%
    // Concentration cap: 10x the FLP per-batch growth as a sane default
    // (production should set per market based on historical OI distribution).
    maxPositionLotsPerTrader: new BN(0), // 0 = unlimited; opt-in by markets
    // Quorum dispersion: 50 bps = 0.5%. If 3 oracles disagree by more
    // than this, reject the update (sources are seeing different markets).
    oracleQuorumMaxDispersionBps: 50,
    // Capital-relative cap: 0 = unlimited. Set per market based on the
    // FLP's tolerable concentration risk (e.g. 100 = 1% of pool).
    maxPositionRatioBps: 0,

    // Liquidation incentives (off by default; opt-in per market).
    liquidatorRewardBps: 0,
    liquidationCooldownSlots: 0,
    liquidationAuctionDurationSlots: 0,
    // JIT auction (off by default).
    jitBonusRebateBps: 0,
    // Affiliate / builder / creator (off by default).
    referrerShareBps: 0,
    builderShareBps: 0,
    creatorShareBps: 0,
    // Pre-launch flag (off — set true only for pre-TGE markets).
    isPreLaunch: false,
  };
}

/**
 * Spot-market parameter recipe. Spot markets use the same matcher as
 * perps but configured to disable funding (k=0), force 1x leverage, and
 * widen the FLP spread modestly. No code-level "spot mode" exists —
 * spot is just a parameter shape that makes the perp engine behave
 * like a spot market. This is by design: the FBA matcher, FLP quoter,
 * commit-reveal, VPIN, and risk modules are all inherently
 * leverage-agnostic.
 */
export function defaultSpotMarketParams(): MarketParamsRaw {
  return {
    ...defaultMajorMarketParams(),
    // No leverage — spot positions are 1:1 collateral-backed.
    maxLeverage: 1,
    // Higher initial margin requirement = no leverage.
    initialMarginRatioBps: 10_000, // 100% — full collateralization
    maintenanceMarginRatioBps: 9_500, // 95% — small buffer for fees
    // No funding (spot has no perpetual funding mechanism).
    fundingRateKBps: 0,
    fundingRateMaxBpsPerSec: 0,
    // No liquidation penalty (positions are fully collateralized).
    liqPenaltyBps: 0,
    // Slightly tighter FLP spread for spot — less variance to hedge.
    flpSpreadBaseBps: 3,
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
