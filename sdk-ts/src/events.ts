// Event types emitted by the Flash Book program. Match the Anchor
// `#[event]` declarations in programs/flash-book/src/lib.rs.

import type { PublicKey } from '@solana/web3.js';
import type BN from 'bn.js';

export interface MarketInitializedEvent {
  market: PublicKey;
  authority: PublicKey;
  initialOracleTicks: BN;
}

export interface BatchClearedEvent {
  market: PublicKey;
  batchNum: BN;
  clearingPrice: BN;
  clearingVolume: BN;
  fillCount: number;
  fundingRateBpsPerSec: BN;
  seizedBonds: BN;
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

// ─── Wave 21 — wrapper-program CPI events ───────────────────────────

export interface OrderPlacedV2CpiEvent extends OrderPlacedV2Event {
  cpiAuthority: PublicKey;
}

export interface WrapperCollateralReleasedEvent {
  cpiAuthority: PublicKey;
  user: PublicKey;
  amount: BN;
}

export interface WrapperTraderStateOpenedEvent {
  cpiAuthority: PublicKey;
  trader: PublicKey;
}

export interface WrapperCollateralCreditedEvent {
  cpiAuthority: PublicKey;
  trader: PublicKey;
  amount: BN;
  newCollateral: BN;
}

export interface WrapperCollateralDebitedEvent {
  cpiAuthority: PublicKey;
  trader: PublicKey;
  amount: BN;
  newCollateral: BN;
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

/// Sequencer feed — emitted by `run_batch_v2` per cleared fill so the
/// off-chain sequencer can dispatch apply_fill / apply_flp_fill on
/// mainnet. FLP detection: `maker == PublicKey.default`.
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

export type FlashBookEvent =
  | { name: 'MarketInitializedEvent'; data: MarketInitializedEvent }
  | { name: 'BatchClearedEvent'; data: BatchClearedEvent }
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
  | { name: 'OrderPlacedV2CpiEvent'; data: OrderPlacedV2CpiEvent }
  | { name: 'WrapperCollateralReleasedEvent'; data: WrapperCollateralReleasedEvent }
  | { name: 'WrapperTraderStateOpenedEvent'; data: WrapperTraderStateOpenedEvent }
  | { name: 'WrapperCollateralCreditedEvent'; data: WrapperCollateralCreditedEvent }
  | { name: 'WrapperCollateralDebitedEvent'; data: WrapperCollateralDebitedEvent }
  | { name: 'FeeTiersInitializedEvent'; data: FeeTiersInitializedEvent }
  | { name: 'FeeTiersUpdatedEvent'; data: FeeTiersUpdatedEvent }
  | { name: 'TraderEffectiveTierEvent'; data: TraderEffectiveTierEvent }
  | { name: 'TraderTierUpgradedEvent'; data: TraderTierUpgradedEvent }
  | { name: 'BatchFillIntentEvent'; data: BatchFillIntentEvent }
  | { name: 'MarketLeverageTiersInitializedEvent'; data: MarketLeverageTiersInitializedEvent }
  | { name: 'MarketLeverageTiersUpdatedEvent'; data: MarketLeverageTiersUpdatedEvent };
