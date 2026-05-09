// Flash Book bot suite — public API.
//
// This package is intentionally separate from the on-chain orderbook code
// (programs/flash-book) and the SDK (sdk-ts). It depends ON the SDK but
// the SDK has no knowledge of bots.
//
// Three production-grade systems live here:
//
//   • MarketMaker — Avellaneda-Stoikov + VPIN-aware quoting against any
//     Venue (Flash Book V3 today; Flash V2 via FlashV2Venue; Phoenix or
//     other CLOBs by implementing the Venue interface).
//
//   • Keepers — liquidation, funding settlement, invariant monitoring,
//     ATA cleanup. Watch chain state, fire the right on-chain ix.
//
//   • Backtester — replays historical fills through the same Strategy
//     class for parameter tuning before risking live capital. (Wave 2.)
//
// All bot code is venue-agnostic by design — the Venue interface is the
// only seam. Re-exporting the SDK here keeps consumers' imports flat.

// ─── SDK convenience re-exports ──────────────────────────────────────
// Consumers who only depend on @flash-book/bot get the SDK client +
// account fetchers without an explicit second import.
export {
  FlashBookClient,
  TOKEN_PROGRAM_ID,
} from '../../sdk-ts/src/client.ts';
export {
  associatedTokenAddress,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  FLASH_BOOK_PROGRAM_ID,
  marketPda,
  orderBufferPda,
  insuranceFundPda,
  flpExposurePda,
  traderStatePda,
  positionPda,
  lpPositionPda,
  type DerivedPda,
} from '../../sdk-ts/src/pdas.ts';

// ─── Bot framework primitives ────────────────────────────────────────
export type {
  MarketSnapshot,
  TraderSnapshot,
  PositionSnapshot,
  QuoteState,
  LiveQuote,
  QuoteAction,
  QuoteParams,
  RiskLimits,
  MarketParams as BotMarketParams,
  Venue,
  MarketBotState,
  BotMarketStats,
  BotStats,
} from './types.ts';
export { computeQuote, type QuoteOutput, type ComputeQuoteInput } from './quote.ts';
export { checkRiskGates, mergeRiskLimits, type RiskGateOutput, type CheckRiskInput } from './risk.ts';
export { diffQuotes, type DiffDecision, type DiffInput } from './diff.ts';

// ─── Multi-market bot (recommended) ──────────────────────────────────
export { Strategy, type StrategyConfig, type StrategyInput, type StrategyOutput } from './strategy.ts';
export { MultiMarketBot, type MultiMarketBotConfig } from './multi-market.ts';
export {
  Backtester,
  type FillEvent,
  type MarketTape,
  type BacktestConfig,
  type BacktestResult,
} from './backtester.ts';

// ─── Single-market MM bot (legacy single-bot path) ───────────────────
export {
  MarketMaker,
  FlashBookVenue,
  type MarketMakerConfig,
  type MarketMakerStats,
} from './market-maker.ts';

// ─── Flash V2 venue adapter ──────────────────────────────────────────
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

// ─── Advanced order types (OCO / Iceberg / Trailing) ────────────────
export {
  OcoOrder,
  IcebergOrder,
  TrailingStopOrder,
  type OcoConfig,
  type OcoState,
  type IcebergConfig,
  type IcebergState,
  type TrailingStopConfig,
  type TrailingStopState,
  type OrderTypeAction,
  type OrderTypeObservation,
  type Side as OrderTypeSide,
} from './order-types.ts';

// ─── WebSocket subscriptions ─────────────────────────────────────────
export {
  subscribeAccount,
  subscribeProgram,
  SubscriptionManager,
  type AccountSubscription,
  type SubscriptionOptions,
} from './subscriptions.ts';

// ─── Smart router (multi-venue) ──────────────────────────────────────
export { SmartRouter, type SmartRouterConfig, type RoutingPolicy } from './smart-router.ts';

// ─── Keeper auto-discovery ───────────────────────────────────────────
export {
  discoverActivePositions,
  discoverEmptyTraderStates,
  type DiscoveryConfig,
  type DiscoveredPosition,
  type DiscoveredTraderState,
} from './discovery.ts';

// ─── Keeper suite ────────────────────────────────────────────────────
export {
  Keeper,
  LiquidationKeeper,
  FundingKeeper,
  InvariantMonitor,
  AtaCleanupKeeper,
  estimateFundingOwed,
  type KeeperBaseConfig,
  type KeeperStats,
  type LiquidationKeeperConfig,
  type FundingKeeperConfig,
  type InvariantMonitorConfig,
  type AtaCleanupKeeperConfig,
} from './keepers.ts';
