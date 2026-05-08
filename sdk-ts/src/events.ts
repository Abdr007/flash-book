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

export interface LiquidationInjectedEvent {
  market: PublicKey;
  trader: PublicKey;
  side: number;
  sizeLots: BN;
  limitTicks: BN;
  worstScenarioIdx: number;
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
  | { name: 'LiquidationInjectedEvent'; data: LiquidationInjectedEvent }
  | { name: 'FlpFillAppliedEvent'; data: FlpFillAppliedEvent }
  | { name: 'MarketStatusChangedEvent'; data: MarketStatusChangedEvent }
  | { name: 'MarketParamsUpdatedEvent'; data: MarketParamsUpdatedEvent }
  | { name: 'MarketAuthorityTransferredEvent'; data: MarketAuthorityTransferredEvent };
