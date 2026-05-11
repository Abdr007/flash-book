// Public API of the Flash Book TypeScript SDK.

export {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ATA_PROGRAM_ID,
  FLASH_BOOK_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  flpExposurePda,
  insuranceFundPda,
  lpPositionPda,
  marketPda,
  positionPda,
  traderStatePda,
  triggerOrderPda,
  twapOrderPda,
  icebergOrderPda,
  vaultPda,
  vaultPositionPda,
  marketBookPda,
  marketLeverageTiersPda,
  feeTiersPda,
  triggerOrderV3Pda,
  twapOrderV3Pda,
  icebergOrderV3Pda,
  flpExposurePerMarketV3Pda,
  flpPositionV3Pda,
  vaultV3Pda,
  vaultPositionV3Pda,
  MAGICBLOCK_DELEGATION_PROGRAM_ID,
  delegateBufferPda,
  delegationRecordPda,
  delegationMetadataPda,
  type DerivedPda,
} from './pdas.ts';

export {
  defaultMajorMarketParams,
  defaultInsuranceFundParams,
  defaultSpotMarketParams,
  type MarketParamsRaw,
  type InsuranceFundInitParams,
} from './params.ts';

export {
  FlashBookClient,
  IDL,
} from './client.ts';

export {
  fetchMarket,
  fetchInsuranceFund,
  fetchFlpExposure,
  fetchTraderState,
  fetchPosition,
  decodeAccount,
  type MarketAccount,
  type InsuranceFundAccount,
  type FlpExposureAccount,
  type TraderStateAccount,
  type PositionAccount,
  type FlpMarketExposure,
  type VpinState,
  type MarketParamsAccount,
} from './accounts.ts';

export {
  decodeEventsFromLogs,
  decodeOne,
  type EventSubscription,
  type EventStreamCallback,
} from './event-decoder.ts';

/// Bitfield flag constants for `placeLimitOrderIx({ flags })` /
/// `placeTakerOrderV2Ix({ flags })`.
/// Bits 0-3 are order semantics (post_only, reduce_only, ioc, jit).
/// Bits 4-5 are STP mode (0=cancel-newest default, 1=cancel-oldest, 2=cancel-both).
/// Bit 6 is FOK (fill-or-kill, CLOB taker only).
export const ORDER_FLAG_POST_ONLY = 1 << 0;
export const ORDER_FLAG_REDUCE_ONLY = 1 << 1;
export const ORDER_FLAG_IOC = 1 << 2;
export const ORDER_FLAG_JIT = 1 << 3;
export const ORDER_FLAG_STP_CANCEL_OLDEST = 1 << 4;
export const ORDER_FLAG_STP_CANCEL_BOTH = 2 << 4;
/// CLOB-only (place_taker_order_v2): require full fill or revert.
export const ORDER_FLAG_FOK = 1 << 6;

export {
  subscribeToProgramEvents,
  subscribeToTraderOrders,
  type TraderOpenOrder,
  type TraderOrderCallbacks,
} from './event-stream.ts';

export {
  defaultScenarios,
  previewPortfolioRisk,
  initialMarginRequired,
  type StressScenario,
  type RiskPreview,
} from './risk-preview.ts';

export {
  FlashBookErrorCode,
  errorFamily,
  errorName,
} from './errors.ts';

// Bot framework (MarketMaker, keepers, venues, multi-market) lives in
// the @flash-book/bot package — see ../bot/. The SDK is intentionally
// ignorant of bots so it stays a clean on-chain client surface.

export type {
  MarketInitializedEvent,
  CollateralDepositedEvent,
  CollateralWithdrawnEvent,
  PartialCollateralWithdrawnEvent,
  FillAppliedEvent,
  FlpFillAppliedEvent,
  MarketStatusChangedEvent,
  MarketParamsUpdatedEvent,
  MarketAuthorityTransferredEvent,
  FlpExposureInitializedEvent,
  FlpCapitalUpdatedEvent,
  MarketBookInitializedEvent,
  OrderPlacedV2Event,
  BookLevelV2,
  BookDepthV2Event,
  OrderCancelledV2Event,
  FeeTiersInitializedEvent,
  FeeTiersUpdatedEvent,
  TraderEffectiveTierEvent,
  TraderTierUpgradedEvent,
  BatchFillIntentEvent,
  TakerOrderClearedEvent,
  MarketLeverageTiersInitializedEvent,
  MarketLeverageTiersUpdatedEvent,
  FlashBookEvent,
} from './events.ts';

/// Compose the encoded `order_id` used by the v2 hypertree book.
/// Mirrors `encode_order_id` in programs/flash-book/src/state_v2.rs:
/// high 48 bits = price, low 16 bits = seq mod 2^16, then bid-side gets
/// the whole u64 inverted so a single ascending sort serves both books.
/// Pass to `cancelOrderV2Ix({ orderId: ... })`.
export function encodeOrderIdV2(
  priceTicks: bigint,
  seq: bigint,
  sideIsBid: boolean,
): bigint {
  const PRICE_MASK = (1n << 48n) - 1n;
  const SEQ_MASK = (1n << 16n) - 1n;
  const U64_MASK = (1n << 64n) - 1n;
  const price = priceTicks & PRICE_MASK;
  const seqLow = seq & SEQ_MASK;
  const raw = (price << 16n) | seqLow;
  return sideIsBid ? raw ^ U64_MASK : raw;
}
export { MarketStatus } from './events.ts';
