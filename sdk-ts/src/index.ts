// Public API of the Flash Book TypeScript SDK.

export {
  FLASH_BOOK_PROGRAM_ID,
  marketPda,
  orderBufferPda,
  commitBufferPda,
  insuranceFundPda,
  flpExposurePda,
  traderStatePda,
  positionPda,
  type DerivedPda,
} from './pdas.ts';

export {
  defaultMajorMarketParams,
  defaultInsuranceFundParams,
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

export { subscribeToProgramEvents } from './event-stream.ts';

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
  FlashBookEvent,
} from './events.ts';
export { MarketStatus } from './events.ts';
