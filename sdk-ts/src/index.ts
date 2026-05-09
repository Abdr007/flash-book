// Public API of the Flash Book TypeScript SDK.

export {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ATA_PROGRAM_ID,
  FLASH_BOOK_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  commitBufferPda,
  flpExposurePda,
  insuranceFundPda,
  lpPositionPda,
  marketPda,
  orderBufferPda,
  positionPda,
  traderStatePda,
  triggerOrderPda,
  twapOrderPda,
  icebergOrderPda,
  vaultPda,
  vaultPositionPda,
  marketBondPda,
  marketBookPda,
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
  fetchOrderBuffer,
  fetchCommitBuffer,
  fetchInsuranceFund,
  fetchFlpExposure,
  fetchTraderState,
  fetchPosition,
  decodeAccount,
  type MarketAccount,
  type OrderBufferAccount,
  type CommitBufferAccount,
  type InsuranceFundAccount,
  type FlpExposureAccount,
  type TraderStateAccount,
  type PositionAccount,
  type OrderSlot,
  type CommitRow,
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

/// Bitfield flag constants for `placeLimitOrderIx({ flags })`.
/// Bits 0-3 are order semantics (post_only, reduce_only, ioc, jit).
/// Bits 4-5 are STP mode (0=cancel-newest default, 1=cancel-oldest, 2=cancel-both).
export const ORDER_FLAG_POST_ONLY = 1 << 0;
export const ORDER_FLAG_REDUCE_ONLY = 1 << 1;
export const ORDER_FLAG_IOC = 1 << 2;
export const ORDER_FLAG_JIT = 1 << 3;
export const ORDER_FLAG_STP_CANCEL_OLDEST = 1 << 4;
export const ORDER_FLAG_STP_CANCEL_BOTH = 2 << 4;

export { subscribeToProgramEvents } from './event-stream.ts';

export {
  defaultScenarios,
  previewPortfolioRisk,
  initialMarginRequired,
  type StressScenario,
  type RiskPreview,
} from './risk-preview.ts';

export {
  simulateBatchClearing,
  fillForOrder,
  SIM_PRIORITY,
  type SimSide,
  type SimOrder,
  type SimFill,
  type SimResult,
} from './order-simulator.ts';

export {
  previewTrade,
  projectPosition,
  type PreviewTradeRequest,
  type PreviewTradeResult,
} from './preview-trade.ts';

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
  BatchClearedEvent,
  CollateralDepositedEvent,
  CollateralWithdrawnEvent,
  FillAppliedEvent,
  FlpFillAppliedEvent,
  LiquidationInjectedEvent,
  MarketStatusChangedEvent,
  MarketParamsUpdatedEvent,
  MarketAuthorityTransferredEvent,
  FlpExposureInitializedEvent,
  FlpCapitalUpdatedEvent,
  OrderCancelledEvent,
  MarketBookInitializedEvent,
  OrderPlacedV2Event,
  BookLevelV2,
  BookDepthV2Event,
  FlashBookEvent,
} from './events.ts';
export { MarketStatus } from './events.ts';
