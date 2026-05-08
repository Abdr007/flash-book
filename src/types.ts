// Types for the Flash Book engine.
//
// All quantities are in human units (not lots) for the simulator —
// production matcher would use integer lot space.
// Prices are in USD per base unit.
// Sizes are in base units.

export type Side = 'long' | 'short';

export type OrderType =
  | 'limit'         // resting maker quote
  | 'taker'         // immediate-cross taker (already revealed)
  | 'liquidation'   // injected by risk engine
  | 'flp_virtual'   // synthesized FLP quote
  | 'adl';          // auto-deleveraging

export interface Order {
  readonly id: string;
  readonly market: string;
  readonly trader: string;
  readonly side: Side;
  readonly size: number;
  readonly limitPrice: number;
  readonly type: OrderType;
  readonly timestamp: number;
  readonly postOnly: boolean;
}

export interface Fill {
  readonly market: string;
  readonly takerId: string;
  readonly makerId: string;
  readonly takerTrader: string;
  readonly makerTrader: string;
  readonly takerSide: Side;
  readonly size: number;
  readonly price: number;
  readonly timestamp: number;
  readonly takerFee: number;
  readonly makerRebate: number;
  readonly toxicityTax: number;
  readonly batchNum: number;
}

export interface Position {
  readonly trader: string;
  readonly market: string;
  side: Side;
  size: number;
  entryPrice: number;
  collateral: number;
  cumFundingIndexAtEntry: number;
  realizedPnl: number;
  fundingPaid: number;
}

export interface FlpState {
  totalCapital: number;
  netPositionByMarket: Map<string, { side: Side; size: number; entryPrice: number }>;
  realizedPnl: number;
}

export interface MarketParams {
  readonly tickSize: number;
  readonly minLotSize: number;

  readonly takerFeeBps: number;
  readonly makerRebateBps: number;
  readonly toxicityTaxMaxBps: number;

  readonly liqPenaltyBps: number;
  readonly maintenanceMarginRatio: number;
  readonly initialMarginRatio: number;
  readonly maxLeverage: number;

  readonly fundingRateMaxPerSec: number;
  readonly fundingRateK: number;

  readonly oracleBandBps: number;

  readonly flpSpreadBaseBps: number;
  readonly flpSpreadAlpha: number;       // VPIN coefficient — adverse selection
  readonly flpSpreadBeta: number;        // pool utilization coefficient
  readonly flpSpreadGamma: number;       // OI imbalance coefficient
  readonly flpSpreadKappa: number;       // depth amortization (Q / depth_floor)
  readonly flpSpreadDelta: number;       // realized volatility coefficient
  readonly flpRiskAversion: number;      // Avellaneda-Stoikov γ — inventory risk aversion
  readonly flpInventoryLambda: number;   // base inventory skew strength
  readonly flpDepthFloor: number;
  readonly flpMaxGrowthPerBatchPct: number;
  readonly flpQuoteLevels: number;

  readonly vpinBucketSize: number;
  readonly vpinEmaWindow: number;

  readonly twapWindow: number;
  readonly batchIntervalMs: number;
}

export interface MarketState {
  readonly symbol: string;
  oraclePrice: number;
  oracleConfidence: number;
  markPrice: number;
  cumFundingIndex: number;
  lastFundingRate: number;
  vpin: number;
  openInterestLong: number;
  openInterestShort: number;
  recentClearingPrices: number[];
  totalFeesCollected: number;
  totalToxicityTaxCollected: number;
  totalLiquidationsCount: number;
  readonly params: MarketParams;
  readonly bidBook: Map<number, Order[]>;
  readonly askBook: Map<number, Order[]>;
}

export interface CommitEntry {
  readonly hash: string;
  readonly trader: string;
  readonly market: string;
  readonly bondLamports: number;
  readonly committedAtBatch: number;
  readonly expireAtBatch: number;
}

export interface BatchInput {
  readonly batchNum: number;
  readonly nowMs: number;
  readonly orders: ReadonlyArray<Order>;
}

export interface BatchResult {
  readonly batchNum: number;
  readonly nowMs: number;
  readonly perMarket: Map<string, MarketBatchResult>;
  readonly liquidations: ReadonlyArray<LiquidationEvent>;
  readonly adl: ReadonlyArray<AdlEvent>;
  readonly insuranceFundDelta: number;
  readonly invariantsHeld: boolean;
}

export interface MarketBatchResult {
  readonly market: string;
  readonly clearingPrice: number;
  readonly clearingVolume: number;
  readonly fills: ReadonlyArray<Fill>;
  readonly markPriceAfter: number;
  readonly fundingRateAfter: number;
  readonly vpinAfter: number;
  readonly flpQuotesUsed: number;
  readonly flpQuotesGenerated: number;
}

export interface LiquidationEvent {
  readonly trader: string;
  readonly market: string;
  readonly side: Side;
  readonly size: number;
  readonly liquidationPrice: number;
  readonly collateralRecovered: number;
  readonly insuranceFundContribution: number;
  readonly bankruptShortfall: number;
  readonly batchNum: number;
}

export interface AdlEvent {
  readonly trader: string;
  readonly market: string;
  readonly side: Side;
  readonly size: number;
  readonly price: number;
  readonly forcedExitReason: 'insurance_exhausted';
  readonly batchNum: number;
}

export interface InsuranceFund {
  balance: number;
  readonly feeContributionRate: number;
  readonly toxicityTaxContributionRate: number;
  readonly liqPenaltyContributionRate: number;
  totalContributions: number;
  totalPayouts: number;
  pauseNewPositionsBelow: number;
}

export interface StressScenario {
  readonly name: string;
  readonly shocks: ReadonlyMap<string, number>;
}

export interface MarginAssessment {
  readonly required: number;
  readonly collateral: number;
  readonly equity: number;
  readonly isHealthy: boolean;
  readonly worstScenario: string;
  readonly worstLoss: number;
}

export interface EngineConfig {
  readonly scenarios: ReadonlyArray<StressScenario>;
  readonly insuranceFund: {
    readonly initialBalance: number;
    readonly feeContributionRate: number;
    readonly toxicityTaxContributionRate: number;
    readonly liqPenaltyContributionRate: number;
    readonly pauseNewPositionsBelow: number;
  };
  readonly commitRevealEnabled: boolean;
  readonly commitExpiryBatches: number;
  readonly commitBondLamports: number;
}

export const FLP_TRADER_ID = 'FLP_POOL';
export const ADL_COUNTERPARTY_ID = 'ADL';
export const INSURANCE_TRADER_ID = 'INSURANCE_FUND';
