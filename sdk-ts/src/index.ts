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

export {
  computeQuote,
  checkRiskGates,
  MarketMaker,
  FlashBookVenue,
  type Venue,
  type MarketSnapshot,
  type TraderSnapshot,
  type PositionSnapshot,
  type QuoteParams,
  type QuoteOutput,
  type RiskLimits,
  type RiskGateOutput,
  type MarketMakerConfig,
  type MarketMakerStats,
} from './market-maker.ts';

export {
  FlashV2Venue,
  V2_SIDE_LONG,
  V2_SIDE_SHORT,
  type FlashV2VenueConfig,
  type MagicTradeClient,
  type V2Side,
  type V2OraclePrice,
  type V2PoolConfig,
  type V2PlaceLimitOrderParams,
  type V2EditLimitOrderParams,
  type V2InstructionResult,
  type V2BasketAccount,
  type V2MarketAccount,
  type V2PositionMeta,
  type V2OrderMeta,
} from './flash-v2-venue.ts';

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
  FlashBookEvent,
} from './events.ts';
export { MarketStatus } from './events.ts';
