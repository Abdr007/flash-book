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

export type FlashBookEvent =
  | { name: 'MarketInitializedEvent'; data: MarketInitializedEvent }
  | { name: 'BatchClearedEvent'; data: BatchClearedEvent }
  | { name: 'CollateralDepositedEvent'; data: CollateralDepositedEvent }
  | { name: 'CollateralWithdrawnEvent'; data: CollateralWithdrawnEvent }
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
  | { name: 'OrderCancelledV2Event'; data: OrderCancelledV2Event };
