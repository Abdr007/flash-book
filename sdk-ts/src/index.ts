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

/// Wave 18h signal: which orderbook the protocol prefers as of this
/// SDK version. New integrations should target v2 (hypertree, resting
/// orders, smarter matcher); v1 (flat array, single-batch) is being
/// deprecated. Wave 19 will delete v1 entirely once trigger / TWAP /
/// iceberg / liquidation flows have v2 equivalents.
export const PREFERRED_ORDERBOOK_VERSION: 'v2' = 'v2';

/// Runtime helper: given a Solana connection + market PDA, ask the
/// chain which orderbook(s) exist for that market. Off-chain
/// sequencers / MMs / dashboards use this to pick the right ix path
/// during the v1→v2 transition. Returns:
///   • 'v2'      — only v2 hypertree exists (use *_v2 ixs)
///   • 'v1'      — only v1 flat buffer exists (use legacy ixs)
///   • 'both'    — both exist (use v2; v1 was likely an earlier init)
///   • 'neither' — market is fresh; init one of the books first
export async function detectOrderbookVersion(
  connection: import('@solana/web3.js').Connection,
  market: import('@solana/web3.js').PublicKey,
  programId?: import('@solana/web3.js').PublicKey,
): Promise<'v1' | 'v2' | 'both' | 'neither'> {
  const v1Pda = (await import('./pdas.ts')).orderBufferPda(market, programId);
  const v2Pda = (await import('./pdas.ts')).marketBookPda(market, programId);
  const [v1Info, v2Info] = await Promise.all([
    connection.getAccountInfo(v1Pda.address),
    connection.getAccountInfo(v2Pda.address),
  ]);
  const v1Live = v1Info !== null && v1Info.data.length > 0;
  const v2Live = v2Info !== null && v2Info.data.length > 0;
  if (v1Live && v2Live) return 'both';
  if (v2Live) return 'v2';
  if (v1Live) return 'v1';
  return 'neither';
}

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
  OrderCancelledV2Event,
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
