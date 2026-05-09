// Pure strategy class — multi-market quote decisions, no I/O.
//
// `Strategy` consumes per-market snapshots + the trader snapshot and emits
// a list of `QuoteAction`s for the executor to apply. The strategy is
// venue-agnostic and side-effect-free: same code runs in production
// (against a live Venue) and in the backtester (against historical fills).
//
// Architecture rationale (drawing from production CLOBs):
//   • Hyperliquid: per-market state isolation; one breakdown doesn't
//     halt other markets. We mirror this — each market in the strategy
//     can independently fail/skip without blocking others.
//   • Drift: cross-margin risk gates fire BEFORE per-market quoting
//     (collateral floor, drawdown). Here too — global gates first.
//   • Phoenix: stateless ix builders. Strategy emits structured actions;
//     the executor turns them into ix.
//
// The Strategy holds in-memory state (last live quote per market, running
// stats) so it can do quote diffing. State is intentionally NOT persisted
// — restart resyncs from on-chain state on next iterate().

import type { PublicKey } from '@solana/web3.js';
import { computeQuote, type QuoteOutput } from './quote.ts';
import { checkRiskGates, mergeRiskLimits } from './risk.ts';
import { diffQuotes } from './diff.ts';
import type {
  BotMarketStats,
  BotStats,
  LiveQuote,
  MarketBotState,
  MarketParams,
  MarketSnapshot,
  PositionSnapshot,
  QuoteAction,
  QuoteParams,
  RiskLimits,
  TraderSnapshot,
} from './types.ts';

export interface StrategyConfig {
  /// Trader pubkey (the bot's identity).
  trader: PublicKey;
  /// Per-market params (one entry per market the bot quotes on).
  markets: ReadonlyArray<MarketParams>;
  /// Global risk limits — apply across all markets.
  globalRiskLimits: RiskLimits;
}

export interface StrategyInput {
  trader: TraderSnapshot;
  markets: ReadonlyMap<string, MarketSnapshot>; // key = market.toBase58()
  positions: ReadonlyMap<string, PositionSnapshot | null>;
  openOrderSeqs: ReadonlyMap<string, ReadonlyArray<bigint>>;
}

export interface StrategyOutput {
  actions: QuoteAction[];
  killSwitchActive: boolean;
  perMarket: BotMarketStats[];
}

export class Strategy {
  private readonly state: Map<string, MarketBotState>;
  private readonly stats: Map<string, BotMarketStats>;
  /// Sum of |signedSize| × mark across markets — used to enforce a
  /// global notional cap by skipping the least-utilized market when the
  /// budget is exhausted.
  private lastTotalNotional = 0n;

  constructor(private readonly config: StrategyConfig) {
    this.state = new Map();
    this.stats = new Map();
    for (const m of config.markets) {
      const key = m.market.toBase58();
      this.state.set(key, {
        market: m.market,
        marketSnap: null,
        positionSnap: null,
        liveQuote: { bidTicks: null, askTicks: null, sizeLots: 0n },
        openSeqs: [],
        unchangedIterations: 0,
      });
      this.stats.set(key, {
        market: key,
        iterationsCompleted: 0,
        ordersPlaced: 0,
        ordersCancelled: 0,
        noopsSkipped: 0,
        txErrors: 0,
        lastInventory: 0n,
        lastQuote: null,
      });
    }
  }

  /// Pure decision step — given a snapshot of the world, emit actions.
  decide(input: StrategyInput): StrategyOutput {
    const actions: QuoteAction[] = [];
    let killSwitchActive = false;
    let totalNotional = 0n;

    // Compute aggregate signed inventory in quote-lot terms across all
    // markets. Used both for per-market risk gates (per-market inventory
    // is local) and for an aggregate health check.
    for (const m of this.config.markets) {
      const key = m.market.toBase58();
      const pos = input.positions.get(key) ?? null;
      const market = input.markets.get(key) ?? null;
      const state = this.state.get(key)!;
      state.marketSnap = market;
      state.positionSnap = pos;
      state.openSeqs = [...(input.openOrderSeqs.get(key) ?? [])];

      if (pos && market) {
        const mag = pos.signedSizeLots < 0n ? -pos.signedSizeLots : pos.signedSizeLots;
        totalNotional += mag * market.markPriceTicks * market.tickSize;
      }
    }

    // Per-market quoting.
    for (const m of this.config.markets) {
      const key = m.market.toBase58();
      const state = this.state.get(key)!;
      const stat = this.stats.get(key)!;
      stat.iterationsCompleted += 1;

      const market = state.marketSnap;
      const pos = state.positionSnap;
      if (!market) {
        stat.lastError = 'market snapshot missing';
        actions.push({ type: 'noop', market: m.market });
        continue;
      }

      const inventory = pos?.signedSizeLots ?? 0n;
      stat.lastInventory = inventory;

      // Risk gate (per-market limits override global where more restrictive).
      const limits = mergeRiskLimits(this.config.globalRiskLimits, m.riskLimits);
      const gates = checkRiskGates({
        inventorySignedLots: inventory,
        collateralQuoteLots: input.trader.collateralQuoteLots,
        realizedPnlQuoteLots: input.trader.realizedPnlQuoteLots,
        limits,
        quoteSizeLots: m.quoteParams.quoteSizeLots,
      });

      if (gates.killSwitchActive) {
        killSwitchActive = true;
      }

      if (!gates.canQuote) {
        // Wind down this market: cancel any open orders.
        if (state.openSeqs.length > 0) {
          actions.push({ type: 'cancel', market: m.market, seqs: state.openSeqs });
          state.liveQuote = { bidTicks: null, askTicks: null, sizeLots: 0n };
        } else {
          actions.push({ type: 'noop', market: m.market });
        }
        stat.lastError = gates.reason;
        continue;
      }

      // Compute the quote.
      const quote = computeQuote({
        market,
        inventorySignedLots: inventory,
        capitalQuoteLots: input.trader.collateralQuoteLots,
        params: m.quoteParams,
        skipBid: gates.skipBid,
        skipAsk: gates.skipAsk,
      });
      stat.lastQuote =
        quote.empty ? null : { bidTicks: quote.bidTicks, askTicks: quote.askTicks };

      if (quote.empty) {
        stat.lastError = 'quote empty';
        if (state.openSeqs.length > 0) {
          actions.push({ type: 'cancel', market: m.market, seqs: state.openSeqs });
          state.liveQuote = { bidTicks: null, askTicks: null, sizeLots: 0n };
        } else {
          actions.push({ type: 'noop', market: m.market });
        }
        continue;
      }

      // Diff against last live quote — skip if the move is below threshold.
      const proposed = {
        bidTicks: quote.bidTicks,
        askTicks: quote.askTicks,
        sizeLots: m.quoteParams.quoteSizeLots,
      };
      const decision = diffQuotes({
        proposed,
        live: state.liveQuote,
        priceDiffBps: m.priceDiffBps ?? 0,
        sizeDiffBps: m.sizeDiffBps ?? 0,
      });

      if (!decision.shouldRequote) {
        stat.noopsSkipped += 1;
        state.unchangedIterations += 1;
        actions.push({ type: 'noop', market: m.market });
        stat.lastError = undefined;
        continue;
      }

      // Action: cancel any existing orders, then place new ones.
      // (We always cancel-replace as a single semantic; the executor
      // batches these into one tx so atomicity holds.)
      if (state.openSeqs.length > 0) {
        actions.push({
          type: 'edit',
          market: m.market,
          quote: proposed,
          existingSeqs: state.openSeqs,
        });
      } else {
        actions.push({ type: 'place', market: m.market, quote: proposed });
      }
      state.liveQuote = {
        bidTicks: proposed.bidTicks > 0n ? proposed.bidTicks : null,
        askTicks: proposed.askTicks > 0n ? proposed.askTicks : null,
        sizeLots: proposed.sizeLots,
      };
      state.unchangedIterations = 0;
      stat.lastError = undefined;
    }

    this.lastTotalNotional = totalNotional;
    return {
      actions,
      killSwitchActive,
      perMarket: Array.from(this.stats.values()).map((s) => ({ ...s })),
    };
  }

  /// Telemetry — called by the executor after each action it actually
  /// fired against the venue (so stats reflect tx-confirmed reality).
  recordExecution(market: PublicKey, kind: 'placed' | 'cancelled' | 'error'): void {
    const stat = this.stats.get(market.toBase58());
    if (!stat) return;
    if (kind === 'placed') stat.ordersPlaced += 1;
    else if (kind === 'cancelled') stat.ordersCancelled += 1;
    else stat.txErrors += 1;
  }

  /// Aggregate stats snapshot. Backtester + executor use this for telemetry.
  snapshot(killSwitchActive: boolean, lastRealizedPnl: bigint): BotStats {
    const perMarket = Array.from(this.stats.values()).map((s) => ({ ...s }));
    let totalPlaced = 0;
    let totalCancelled = 0;
    let totalNoops = 0;
    let totalErrors = 0;
    let totalIters = 0;
    for (const s of perMarket) {
      totalPlaced += s.ordersPlaced;
      totalCancelled += s.ordersCancelled;
      totalNoops += s.noopsSkipped;
      totalErrors += s.txErrors;
      totalIters = Math.max(totalIters, s.iterationsCompleted);
    }
    return {
      startedAt: 0, // owner sets this externally
      iterationsCompleted: totalIters,
      totalOrdersPlaced: totalPlaced,
      totalOrdersCancelled: totalCancelled,
      totalNoopsSkipped: totalNoops,
      totalTxErrors: totalErrors,
      killSwitchActive,
      lastRealizedPnl,
      perMarket,
    };
  }

  /// Inspect last computed total notional. Used for off-chain dashboards.
  getLastTotalNotional(): bigint {
    return this.lastTotalNotional;
  }

  /// Reset live-quote state for a market (e.g., after manual cancel
  /// outside the bot). Forces the next iteration to re-quote.
  resetMarket(market: PublicKey): void {
    const state = this.state.get(market.toBase58());
    if (!state) return;
    state.liveQuote = { bidTicks: null, askTicks: null, sizeLots: 0n };
    state.unchangedIterations = 0;
  }
}

// Re-exports the QuoteOutput so consumers don't need to import from quote.ts.
export type { QuoteOutput, QuoteParams, MarketParams, RiskLimits };
