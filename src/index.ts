// Flash Book — public API.
//
// Pool-backed CLOB matched by frequent batch auction, designed for
// MagicBlock Ephemeral Rollups, settling to Solana mainnet.

export type {
  AdlEvent,
  BatchInput,
  BatchResult,
  CommitEntry,
  EngineConfig,
  Fill,
  FlpState,
  InsuranceFund,
  LiquidationEvent,
  MarginAssessment,
  MarketBatchResult,
  MarketParams,
  MarketState,
  Order,
  OrderType,
  Position,
  Side,
  StressScenario,
} from './types.ts';

export {
  ADL_COUNTERPARTY_ID,
  FLP_TRADER_ID,
  INSURANCE_TRADER_ID,
} from './types.ts';

export {
  generateFlpQuotes,
  type FlpQuoterInput,
  type FlpQuoterOutput,
} from './flp-quoter.ts';

export {
  advanceFundingIndex,
  fundingOwed,
  settleFunding,
  type FundingTick,
} from './funding.ts';

export {
  assessMargin,
  generateScenarios,
  initialMarginRequired,
} from './risk.ts';

export {
  computeShortfall,
  detectLiquidations,
  generateLiquidationOrders,
  makeLiquidationEvent,
  type LiquidationCandidate,
  type LiquidationOrderGenInput,
} from './liquidation.ts';

export {
  contributeFromFees,
  contributeFromLiqPenalty,
  contributeFromToxicityTax,
  coverShortfall,
  createInsuranceFund,
  newPositionsAllowed,
  recommendedFundSize,
  type BankruptcyResolution,
} from './insurance.ts';

export { VpinCalculator, type VpinSnapshot } from './vpin.ts';

export {
  clamp,
  commitHash,
  emaUpdate,
  oracleBand,
  Prng,
  pushAndTwap,
  roundLot,
  roundToTick,
  safeNumber,
  sumSafe,
} from './math.ts';

// Convenience: a sensible default param set for major pairs.
import type { MarketParams } from './types.ts';

export const DEFAULT_MAJOR_MARKET_PARAMS: MarketParams = {
  tickSize: 0.01,
  minLotSize: 0.001,

  takerFeeBps: 5,
  makerRebateBps: 1,
  toxicityTaxMaxBps: 5,

  liqPenaltyBps: 50,
  maintenanceMarginRatio: 0.0125,
  initialMarginRatio: 0.025,
  maxLeverage: 40,

  fundingRateMaxPerSec: 1e-6, // ~3.6% per hour cap
  fundingRateK: 1 / 3600,      // 1% premium → 1%/hour

  oracleBandBps: 100,           // ±1% of oracle

  flpSpreadBaseBps: 5,
  flpSpreadAlpha: 0.5,
  flpSpreadBeta: 0.3,
  flpSpreadGamma: 0.2,
  flpSpreadKappa: 0.05,
  flpSpreadDelta: 2.0,         // realized-vol coefficient
  flpRiskAversion: 50,          // Avellaneda-Stoikov γ
  flpInventoryLambda: 0.5,
  flpDepthFloor: 1000,
  flpMaxGrowthPerBatchPct: 0.005, // 0.5% of pool per batch
  flpQuoteLevels: 5,

  vpinBucketSize: 100,
  vpinEmaWindow: 50,

  twapWindow: 5,
  batchIntervalMs: 50,
};

// Oracle hardening defaults — sized to current Pyth + cex-aggregator
// observed staleness/confidence on Solana mainnet.
export const DEFAULT_ORACLE_STALENESS_MAX_SECONDS = 30;
export const DEFAULT_ORACLE_CONFIDENCE_MAX_BPS = 100; // 1%
