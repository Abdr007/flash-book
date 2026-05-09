// MultiMarketBot — production executor wrapping the Strategy class.
// Pulls per-market state from the venue, runs Strategy.decide(), then
// applies QuoteActions atomically per-market via the venue.
//
// What's "advanced" here vs the legacy single-market MarketMaker:
//   • Multiple markets quoted in one process with shared inventory budget.
//   • Quote diffing (per-market priceDiffBps / sizeDiffBps) so the bot
//     skips re-quotes that don't move the market.
//   • Per-market risk limit overrides on top of global limits.
//   • Stable in-memory live-quote tracking — no redundant cancel-replace.
//   • Strategy/Executor split — Strategy is pure (testable in backtester);
//     Executor is the only piece touching the network.
//   • Graceful per-market failure isolation.

import { Connection, Keypair, type TransactionInstruction } from '@solana/web3.js';
import type { PublicKey } from '@solana/web3.js';
import { Strategy, type StrategyConfig } from './strategy.ts';
import type {
  BotStats,
  MarketSnapshot,
  PositionSnapshot,
  QuoteAction,
  TraderSnapshot,
  Venue,
} from './types.ts';

export interface MultiMarketBotConfig extends StrategyConfig {
  signer: Keypair;
  /// Re-quote cadence (ms).
  refreshMs: number;
  /// If true, compute decisions + log without sending tx.
  dryRun?: boolean;
  /// Optional callback invoked after each iteration with the current
  /// stats snapshot. Operators wire this to telemetry.
  onIteration?: (stats: BotStats) => void;
}

export class MultiMarketBot {
  private readonly strategy: Strategy;
  private readonly stats: { startedAt: number; killSwitchActive: boolean; lastRealizedPnl: bigint };
  private timer: ReturnType<typeof setInterval> | null = null;
  private busy = false;

  constructor(
    private readonly venue: Venue,
    private readonly connection: Connection,
    private readonly config: MultiMarketBotConfig,
  ) {
    this.strategy = new Strategy(config);
    this.stats = { startedAt: Date.now(), killSwitchActive: false, lastRealizedPnl: 0n };
  }

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => {
      if (this.busy) return;
      void this.iterate();
    }, this.config.refreshMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  getStats(): Readonly<BotStats> {
    const snap = this.strategy.snapshot(this.stats.killSwitchActive, this.stats.lastRealizedPnl);
    return { ...snap, startedAt: this.stats.startedAt };
  }

  /// One iteration. Public for explicit-call testing.
  async iterate(): Promise<void> {
    this.busy = true;
    try {
      // 1. Fetch trader snapshot once (shared across markets).
      const trader = await this.venue.fetchTrader(this.config.trader);
      if (!trader) return;
      this.stats.lastRealizedPnl = trader.realizedPnlQuoteLots;

      // 2. Fetch per-market snapshots in parallel.
      const markets = new Map<string, MarketSnapshot>();
      const positions = new Map<string, PositionSnapshot | null>();
      const openSeqs = new Map<string, ReadonlyArray<bigint>>();
      await Promise.all(
        this.config.markets.map(async (m) => {
          const key = m.market.toBase58();
          const [snap, pos, seqs] = await Promise.all([
            this.venue.fetchMarket(m.market),
            this.venue.fetchPosition(m.market, this.config.trader),
            this.venue.fetchOpenOrderSeqs(m.market, this.config.trader),
          ]);
          if (snap) markets.set(key, snap);
          positions.set(key, pos);
          openSeqs.set(key, seqs);
        }),
      );

      // 3. Run strategy.
      const out = this.strategy.decide({
        trader,
        markets,
        positions,
        openOrderSeqs: openSeqs,
      });
      this.stats.killSwitchActive = out.killSwitchActive;

      // 4. Execute actions per-market (parallel, isolated failure).
      await Promise.all(out.actions.map((action) => this.executeAction(action, trader)));

      this.config.onIteration?.(this.getStats());
    } finally {
      this.busy = false;
    }
  }

  private async executeAction(action: QuoteAction, _trader: TraderSnapshot): Promise<void> {
    if (action.type === 'noop') return;
    try {
      const ixs: TransactionInstruction[] = [];
      if (action.type === 'cancel' || action.type === 'edit') {
        const seqs = action.type === 'edit' ? action.existingSeqs : action.seqs;
        if (seqs.length > 0) {
          ixs.push(
            ...(await this.venue.buildCancelInstructions({
              trader: this.config.trader,
              market: action.market,
              seqs,
            })),
          );
        }
      }
      if (action.type === 'place' || action.type === 'edit') {
        ixs.push(
          ...(await this.venue.buildQuoteInstructions({
            trader: this.config.trader,
            market: action.market,
            bidTicks: action.quote.bidTicks,
            askTicks: action.quote.askTicks,
            sizeLots: action.quote.sizeLots,
          })),
        );
      }
      if (ixs.length === 0) return;
      if (this.config.dryRun) return;
      await this.venue.sendTx(ixs, [this.config.signer]);
      // Telemetry: count one place per non-zero quote leg.
      if (action.type === 'place' || action.type === 'edit') {
        const placed =
          (action.quote.bidTicks > 0n ? 1 : 0) + (action.quote.askTicks > 0n ? 1 : 0);
        for (let i = 0; i < placed; i++) {
          this.strategy.recordExecution(action.market, 'placed');
        }
      }
      if (action.type === 'cancel' || action.type === 'edit') {
        const seqs = action.type === 'edit' ? action.existingSeqs : action.seqs;
        for (let i = 0; i < seqs.length; i++) {
          this.strategy.recordExecution(action.market, 'cancelled');
        }
      }
    } catch {
      this.strategy.recordExecution(action.market, 'error');
    }
  }

  /// Reset live-quote state for a market — caller knows orders were
  /// modified out-of-band.
  resetMarket(market: PublicKey): void {
    this.strategy.resetMarket(market);
  }
}
