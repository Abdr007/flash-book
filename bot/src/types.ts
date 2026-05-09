// Shared types for the bot framework. Pure data — no I/O dependencies.

import type { PublicKey, TransactionInstruction, Keypair } from '@solana/web3.js';

// ─── Snapshot types (mirrored from market-maker.ts for back-compat) ───

export interface MarketSnapshot {
  markPriceTicks: bigint;
  vpinBps: number;
  tickSize: bigint;
  minBaseLots: bigint;
  oiImbalanceLots: bigint;
  oiTotalLots: bigint;
  currentBatch: bigint;
}

export interface TraderSnapshot {
  collateralQuoteLots: bigint;
  realizedPnlQuoteLots: bigint;
  openPositions: number;
}

export interface PositionSnapshot {
  signedSizeLots: bigint;
  entryPriceTicks: bigint;
}

// ─── Quote types ──────────────────────────────────────────────────────

/// A single bid/ask pair we want live on a market.
export interface QuoteState {
  bidTicks: bigint;
  askTicks: bigint;
  sizeLots: bigint;
}

/// What was last placed on chain for a market — used by the diffing logic.
export interface LiveQuote {
  bidTicks: bigint | null;
  askTicks: bigint | null;
  sizeLots: bigint;
}

/// Instruction to issue against a market this iteration.
export type QuoteAction =
  | { type: 'place'; market: PublicKey; quote: QuoteState }
  | { type: 'edit'; market: PublicKey; quote: QuoteState; existingSeqs: bigint[] }
  | { type: 'cancel'; market: PublicKey; seqs: bigint[] }
  | { type: 'noop'; market: PublicKey };

// ─── Strategy params ──────────────────────────────────────────────────

export interface QuoteParams {
  baseSpreadBps: number;
  vpinSpreadAlpha: number;
  inventorySkewBpsPerUnit: number;
  oiImbalanceSpreadCoef: number;
  quoteSizeLots: bigint;
}

export interface RiskLimits {
  maxInventoryLots: bigint;
  maxDrawdownQuoteLots: bigint;
  minCollateralQuoteLots: bigint;
}

/// Per-market strategy config.
export interface MarketParams {
  market: PublicKey;
  quoteParams: QuoteParams;
  /// Per-market risk limits override the global ones when more
  /// restrictive. Optional.
  riskLimits?: RiskLimits;
  /// Diff thresholds — re-quote only when the new prices move past these.
  /// Set to 0 to always re-quote (legacy behaviour).
  priceDiffBps?: number;
  sizeDiffBps?: number;
}

// ─── Venue contract (multi-market aware) ──────────────────────────────

export interface Venue {
  readonly name: string;
  fetchMarket(market: PublicKey): Promise<MarketSnapshot | null>;
  fetchTrader(trader: PublicKey): Promise<TraderSnapshot | null>;
  fetchPosition(market: PublicKey, trader: PublicKey): Promise<PositionSnapshot | null>;
  fetchOpenOrderSeqs(market: PublicKey, trader: PublicKey): Promise<bigint[]>;
  buildQuoteInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    bidTicks: bigint;
    askTicks: bigint;
    sizeLots: bigint;
  }): Promise<TransactionInstruction[]>;
  buildCancelInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    seqs: bigint[];
  }): Promise<TransactionInstruction[]>;
  sendTx(instructions: TransactionInstruction[], signers: Keypair[]): Promise<string>;
}

// ─── Per-market state held by the bot ─────────────────────────────────

export interface MarketBotState {
  market: PublicKey;
  marketSnap: MarketSnapshot | null;
  positionSnap: PositionSnapshot | null;
  liveQuote: LiveQuote;
  openSeqs: bigint[];
  /// Number of consecutive iterations a quote stayed inside the diff
  /// window. Useful telemetry for tuning.
  unchangedIterations: number;
}

// ─── Bot stats ────────────────────────────────────────────────────────

export interface BotMarketStats {
  market: string;
  iterationsCompleted: number;
  ordersPlaced: number;
  ordersCancelled: number;
  noopsSkipped: number;
  txErrors: number;
  lastInventory: bigint;
  lastQuote: { bidTicks: bigint; askTicks: bigint } | null;
  lastError?: string | undefined;
}

export interface BotStats {
  startedAt: number;
  iterationsCompleted: number;
  totalOrdersPlaced: number;
  totalOrdersCancelled: number;
  totalNoopsSkipped: number;
  totalTxErrors: number;
  killSwitchActive: boolean;
  lastRealizedPnl: bigint;
  perMarket: BotMarketStats[];
  lastError?: string | undefined;
}
