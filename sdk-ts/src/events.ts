// Event types emitted by the Flash Book program. Match the Anchor
// `#[event]` declarations in programs/flash-book/src/lib.rs.

import type { PublicKey } from '@solana/web3.js';
import type BN from 'bn.js';

export interface MarketInitializedEvent {
  market: PublicKey;
  authority: PublicKey;
  initialOracleTicks: BN;
}

export interface CollateralDepositedEvent {
  trader: PublicKey;
  amount: BN;
  newBalance: BN;
}

export interface CollateralWithdrawnEvent {
  trader: PublicKey;
  amount: BN;
  newBalance: BN;
}

export interface FillAppliedEvent {
  market: PublicKey;
  taker: PublicKey;
  maker: PublicKey;
  takerSide: number;
  sizeLots: BN;
  priceTicks: BN;
  batchNum: BN;
}

export interface FlpExposureInitializedEvent {
  authority: PublicKey;
  initialCapital: BN;
}

export interface FlpCapitalUpdatedEvent {
  newTotal: BN;
  delta: BN;
}

export interface FlpFillAppliedEvent {
  market: PublicKey;
  taker: PublicKey;
  takerSide: number;
  sizeLots: BN;
  priceTicks: BN;
  batchNum: BN;
  flpSizeAfter: BN;
  flpSideAfter: number;
}

export interface MarketStatusChangedEvent {
  market: PublicKey;
  previousStatus: number;
  newStatus: number;
}

export interface MarketParamsUpdatedEvent {
  market: PublicKey;
}

export interface MarketAuthorityTransferredEvent {
  market: PublicKey;
  previousAuthority: PublicKey;
  newAuthority: PublicKey;
}

// V2 hypertree-orderbook events. Mirror the `#[event]` decls in lib.rs.

export interface MarketBookInitializedEvent {
  market: PublicKey;
  marketBook: PublicKey;
  totalBytes: number;
  dataBytes: number;
}

export interface OrderPlacedV2Event {
  market: PublicKey;
  trader: PublicKey;
  seq: BN;
  side: number;
  priceTicks: BN;
  sizeLots: BN;
  nodeIndex: number;
  totalOrdersAfter: number;
}

/// One price level in `BookDepthV2Event`. Mirrors the `BookLevelV2`
/// AnchorSerialize struct in lib.rs.
export interface BookLevelV2 {
  priceTicks: BN;
  sizeLots: BN;
  seq: BN;
  trader: PublicKey;
}

export interface BookDepthV2Event {
  market: PublicKey;
  totalOrdersActive: number;
  bids: BookLevelV2[];
  asks: BookLevelV2[];
}

export interface OrderCancelledV2Event {
  market: PublicKey;
  trader: PublicKey;
  orderSeq: BN;
  side: number;
  nodeIndex: number;
  totalOrdersAfter: number;
}

export enum MarketStatus {
  Inactive = 0,
  Active = 1,
  PostOnly = 2,
  Paused = 3,
  Closed = 4,
}

// ─── Wave 22 — fee-tier events ──────────────────────────────────────

export interface FeeTiersInitializedEvent {
  authority: PublicKey;
  tierCount: number;
  volumeWindowSlots: BN;
}

export interface FeeTiersUpdatedEvent {
  authority: PublicKey;
  tierCount: number;
  volumeWindowSlots: BN;
}

export interface TraderEffectiveTierEvent {
  trader: PublicKey;
  tierIndex: number;
  effectiveVolumeQuoteLots: BN;
  /// SIGNED — positive = maker rebate, negative = maker fee.
  makerRebateBps: number;
  takerFeeBps: number;
  windowExpired: boolean;
}

export interface TraderTierUpgradedEvent {
  trader: PublicKey;
  previousTierIndex: number;
  newTierIndex: number;
  volumeQuoteLots: BN;
}

/// Sequencer feed — emitted inline per CLOB taker match so the off-chain
/// sequencer can dispatch apply_fill / apply_flp_fill on mainnet. FLP
/// detection: `maker == PublicKey.default`.
export interface BatchFillIntentEvent {
  market: PublicKey;
  taker: PublicKey;
  maker: PublicKey;
  takerSide: number;
  sizeLots: BN;
  priceTicks: BN;
  takerId: BN;
  makerId: BN;
}

/// CLOB taker-walk summary — emitted once per `place_taker_order_v2` call
/// after the matcher walks the resting book. Carries the requested vs
/// actually-filled vs residual-resting size + the count of inline
/// `BatchFillIntentEvent`s emitted in the same tx.
export interface TakerOrderClearedEvent {
  market: PublicKey;
  taker: PublicKey;
  takerSide: number;
  takerSizeLots: BN;
  filledLots: BN;
  residualRestingLots: BN;
  matchCount: number;
}

// ─── Wave 20a — multi-tier MMR events ───────────────────────────────

export interface MarketLeverageTiersInitializedEvent {
  market: PublicKey;
  tierCount: number;
}

export interface MarketLeverageTiersUpdatedEvent {
  market: PublicKey;
  tierCount: number;
}

// ─── Wave 20b — partial withdraw event ──────────────────────────────

export interface PartialCollateralWithdrawnEvent {
  trader: PublicKey;
  amount: BN;
  newBalance: BN;
}

// ─── V3 mark-price engine events ────────────────────────────────────

/// `source` byte:
///   0 = `apply_fill` EMA blend (last-trade-price tracking)
///   1 = `settle_mark` hard reset to oracle
///   2 = future paths (forward-compat)
export interface MarkPriceUpdatedEvent {
  market: PublicKey;
  oldMarkTicks: BN;
  newMarkTicks: BN;
  oracleTicks: BN;
  source: number;
}

/// Emitted whenever |mark - oracle| / oracle exceeds `params.driftAlertBps`.
/// Off-chain observers use this as a nudge to call `settleMarkIx`.
export interface MarkPriceDriftEvent {
  market: PublicKey;
  markTicks: BN;
  oracleTicks: BN;
  /// Absolute drift in bps of oracle.
  driftBps: number;
}

/// Emitted by `liquidate_position_v2` so off-chain consumers can
/// distinguish "liquidated by fresh oracle move" (source=1) from
/// "liquidated by mark drift" (source=0). Only fires when the dual-source
/// health gate actually triggers.
export interface HealthGateSourceEvent {
  market: PublicKey;
  trader: PublicKey;
  markTicks: BN;
  oracleTicks: BN;
  healthPriceTicks: BN;
  /// 0 = mark, 1 = oracle, 2 = mark == oracle (rare).
  source: number;
}

export type FlashBookEvent =
  | { name: 'MarketInitializedEvent'; data: MarketInitializedEvent }
  | { name: 'CollateralDepositedEvent'; data: CollateralDepositedEvent }
  | { name: 'CollateralWithdrawnEvent'; data: CollateralWithdrawnEvent }
  | { name: 'PartialCollateralWithdrawnEvent'; data: PartialCollateralWithdrawnEvent }
  | { name: 'FillAppliedEvent'; data: FillAppliedEvent }
  | { name: 'FlpFillAppliedEvent'; data: FlpFillAppliedEvent }
  | { name: 'MarketStatusChangedEvent'; data: MarketStatusChangedEvent }
  | { name: 'MarketParamsUpdatedEvent'; data: MarketParamsUpdatedEvent }
  | { name: 'MarketAuthorityTransferredEvent'; data: MarketAuthorityTransferredEvent }
  | { name: 'FlpExposureInitializedEvent'; data: FlpExposureInitializedEvent }
  | { name: 'FlpCapitalUpdatedEvent'; data: FlpCapitalUpdatedEvent }
  | { name: 'MarketBookInitializedEvent'; data: MarketBookInitializedEvent }
  | { name: 'OrderPlacedV2Event'; data: OrderPlacedV2Event }
  | { name: 'BookDepthV2Event'; data: BookDepthV2Event }
  | { name: 'OrderCancelledV2Event'; data: OrderCancelledV2Event }
  | { name: 'FeeTiersInitializedEvent'; data: FeeTiersInitializedEvent }
  | { name: 'FeeTiersUpdatedEvent'; data: FeeTiersUpdatedEvent }
  | { name: 'TraderEffectiveTierEvent'; data: TraderEffectiveTierEvent }
  | { name: 'TraderTierUpgradedEvent'; data: TraderTierUpgradedEvent }
  | { name: 'BatchFillIntentEvent'; data: BatchFillIntentEvent }
  | { name: 'TakerOrderClearedEvent'; data: TakerOrderClearedEvent }
  | { name: 'MarketLeverageTiersInitializedEvent'; data: MarketLeverageTiersInitializedEvent }
  | { name: 'MarketLeverageTiersUpdatedEvent'; data: MarketLeverageTiersUpdatedEvent }
  | { name: 'MarkPriceUpdatedEvent'; data: MarkPriceUpdatedEvent }
  | { name: 'MarkPriceDriftEvent'; data: MarkPriceDriftEvent }
  | { name: 'HealthGateSourceEvent'; data: HealthGateSourceEvent };
