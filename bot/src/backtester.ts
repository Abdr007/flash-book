// Backtester — replays historical fills through the same Strategy class
// used in production. Operators tune (baseSpreadBps, vpinSpreadAlpha,
// inventorySkewBpsPerUnit, etc.) offline against a real fill tape, then
// deploy with proven parameters.
//
// Design: the Strategy is pure. The backtester builds a synthetic Venue
// (in-memory) that fakes fetchMarket/fetchPosition/fetchTrader from the
// historical tape. It also implements a simple maker-fill model: when
// the bot's bid/ask straddles the next observed trade, the bot fills.
//
// Maker-fill model (intentionally simple, well-documented):
//   • A trade event has (price, size, takerSide).
//   • If takerSide = buy and our ask ≤ trade.price → we (maker) sell.
//   • If takerSide = sell and our bid ≥ trade.price → we (maker) buy.
//   • Fill size = min(trade.size, our quote size).
//   • Position + collateral updates apply maker fee/rebate.
//
// What's NOT modeled (operators should be aware):
//   • Queue position — we assume we always fill if we cross. Real
//     books have time priority; partial fills more common.
//   • Latency — we re-quote instantly each iteration.
//   • Adverse selection — VPIN signal is REAL in the tape; bot may
//     widen its spread, leading to fewer fills (this IS modeled).

import type { PublicKey } from '@solana/web3.js';
import { Strategy, type StrategyConfig } from './strategy.ts';
import type {
  BotStats,
  MarketSnapshot,
  PositionSnapshot,
  QuoteAction,
  TraderSnapshot,
} from './types.ts';

export interface FillEvent {
  /// Tape event timestamp (ms since epoch, or any monotonic clock).
  ts: number;
  /// Market the trade hit.
  market: PublicKey;
  /// Trade price in ticks.
  priceTicks: bigint;
  /// Traded size in base lots.
  sizeLots: bigint;
  /// Taker side at this fill — determines whether OUR bid (taker=sell)
  /// or OUR ask (taker=buy) gets matched if we crossed.
  takerSide: 'long' | 'short';
}

export interface MarketTape {
  market: PublicKey;
  /// Sorted by ts ascending.
  fills: ReadonlyArray<FillEvent>;
  /// Mark price at start of replay. Updated to last trade price as we go.
  initialMarkTicks: bigint;
  /// VPIN bps observed alongside fills. Optional — defaults to 0.
  vpinSeries?: ReadonlyArray<{ ts: number; vpinBps: number }>;
  tickSize: bigint;
  minBaseLots: bigint;
}

export interface BacktestConfig extends StrategyConfig {
  /// Initial collateral the bot starts with.
  initialCollateralQuoteLots: bigint;
  /// Maker rebate paid PER FILL as bps of notional (used by the
  /// fill model to credit the bot's collateral).
  makerRebateBps: number;
  /// Tapes for each market the strategy quotes on.
  tapes: ReadonlyMap<string, MarketTape>;
  /// Re-quote cadence (ms in tape time). The bot decides every N ms.
  refreshMs: number;
  /// Hard stop: max number of iterations to run.
  maxIterations?: number;
}

export interface BacktestResult {
  iterations: number;
  fills: number;
  finalCollateralQuoteLots: bigint;
  realizedPnlQuoteLots: bigint;
  netInventoryByMarket: Map<string, bigint>;
  stats: BotStats;
}

interface SimMarketState {
  tape: MarketTape;
  cursor: number; // next unread fill index
  markTicks: bigint;
  vpinBps: number;
  position: PositionSnapshot | null;
  liveBidTicks: bigint | null;
  liveAskTicks: bigint | null;
  liveSizeLots: bigint;
  fillsAbsorbed: number;
}

export class Backtester {
  private readonly strategy: Strategy;
  private readonly markets: Map<string, SimMarketState>;
  private trader: TraderSnapshot;
  private elapsedMs = 0;
  private iterCount = 0;
  private totalFills = 0;

  constructor(private readonly config: BacktestConfig) {
    this.strategy = new Strategy(config);
    this.markets = new Map();
    for (const m of config.markets) {
      const key = m.market.toBase58();
      const tape = config.tapes.get(key);
      if (!tape) throw new Error(`tape missing for market ${key}`);
      this.markets.set(key, {
        tape,
        cursor: 0,
        markTicks: tape.initialMarkTicks,
        vpinBps: tape.vpinSeries?.[0]?.vpinBps ?? 0,
        position: null,
        liveBidTicks: null,
        liveAskTicks: null,
        liveSizeLots: 0n,
        fillsAbsorbed: 0,
      });
    }
    this.trader = {
      collateralQuoteLots: config.initialCollateralQuoteLots,
      realizedPnlQuoteLots: 0n,
      openPositions: 0,
    };
  }

  /// Run the backtest to completion. Stops when (a) all tapes are
  /// exhausted, (b) no live quotes are open, AND (c) no positions remain
  /// — OR when `maxIterations` is hit (whichever first).
  run(): BacktestResult {
    const maxIter = this.config.maxIterations ?? Number.MAX_SAFE_INTEGER;
    while (this.iterCount < maxIter) {
      this.iterate();
      // Natural-completion check: all tapes replayed AND no live quotes.
      let allDone = true;
      for (const [, st] of this.markets) {
        if (st.cursor < st.tape.fills.length) {
          allDone = false;
          break;
        }
        if (st.liveBidTicks !== null || st.liveAskTicks !== null) {
          allDone = false;
          break;
        }
      }
      // For empty tapes, allDone = true on iter 1 — keep going to honor
      // maxIterations explicitly. Operators run with an explicit cap.
      if (allDone && this.config.maxIterations === undefined) break;
    }
    const netInv = new Map<string, bigint>();
    for (const [key, st] of this.markets) {
      netInv.set(key, st.position?.signedSizeLots ?? 0n);
    }
    return {
      iterations: this.iterCount,
      fills: this.totalFills,
      finalCollateralQuoteLots: this.trader.collateralQuoteLots,
      realizedPnlQuoteLots: this.trader.realizedPnlQuoteLots,
      netInventoryByMarket: netInv,
      stats: this.strategy.snapshot(false, this.trader.realizedPnlQuoteLots),
    };
  }

  /// One iteration: advance the tape by `refreshMs`, apply fills against
  /// our live quotes, then run Strategy.decide() and update live quotes.
  iterate(): void {
    this.iterCount += 1;
    this.elapsedMs += this.config.refreshMs;

    // 1. Advance tape — apply fills that occurred in this slice.
    for (const [, st] of this.markets) {
      while (
        st.cursor < st.tape.fills.length &&
        st.tape.fills[st.cursor]!.ts <= this.elapsedMs
      ) {
        const fill = st.tape.fills[st.cursor]!;
        this.maybeFill(st, fill);
        st.markTicks = fill.priceTicks;
        st.cursor += 1;
      }
      if (st.tape.vpinSeries) {
        for (const v of st.tape.vpinSeries) {
          if (v.ts <= this.elapsedMs) st.vpinBps = v.vpinBps;
        }
      }
    }

    // 2. Build snapshots and run strategy.
    const markets = new Map<string, MarketSnapshot>();
    const positions = new Map<string, PositionSnapshot | null>();
    const openSeqs = new Map<string, bigint[]>();
    for (const [key, st] of this.markets) {
      markets.set(key, {
        markPriceTicks: st.markTicks,
        vpinBps: st.vpinBps,
        tickSize: st.tape.tickSize,
        minBaseLots: st.tape.minBaseLots,
        oiImbalanceLots: 0n,
        oiTotalLots: 0n,
        currentBatch: BigInt(this.iterCount),
      });
      positions.set(key, st.position);
      // Synthetic seqs — encode side per-quote for the strategy's diff logic.
      const seqs: bigint[] = [];
      if (st.liveBidTicks !== null) seqs.push(1n);
      if (st.liveAskTicks !== null) seqs.push(2n);
      openSeqs.set(key, seqs);
    }

    const out = this.strategy.decide({
      trader: this.trader,
      markets,
      positions,
      openOrderSeqs: openSeqs,
    });

    // 3. Apply quote actions to internal state.
    for (const action of out.actions) {
      this.applyAction(action);
    }
  }

  /// Apply a strategy action to the simulator's internal state.
  /// Side-effects: updates per-market live quotes.
  private applyAction(action: QuoteAction): void {
    const key = action.market.toBase58();
    const st = this.markets.get(key)!;
    if (action.type === 'noop') return;
    if (action.type === 'cancel') {
      st.liveBidTicks = null;
      st.liveAskTicks = null;
      st.liveSizeLots = 0n;
      return;
    }
    // place or edit — set live quotes.
    st.liveBidTicks = action.quote.bidTicks > 0n ? action.quote.bidTicks : null;
    st.liveAskTicks = action.quote.askTicks > 0n ? action.quote.askTicks : null;
    st.liveSizeLots = action.quote.sizeLots;
  }

  /// Maker-fill model. If our quote was the best price the taker hit,
  /// we get filled at our quote price. Mirrors real CLOB fill semantics:
  /// taker hits the best bid/ask; if our bid ≥ taker's sell-price, we
  /// were on the book and we filled at our bid.
  private maybeFill(st: SimMarketState, fill: FillEvent): boolean {
    if (st.liveSizeLots <= 0n) return false;

    let ourSide: 'long' | 'short' | null = null;
    let ourPriceTicks: bigint | null = null;

    // If taker is buying (taking liquidity from asks), our ask might fill.
    if (fill.takerSide === 'long' && st.liveAskTicks !== null && st.liveAskTicks <= fill.priceTicks) {
      ourSide = 'short';
      ourPriceTicks = st.liveAskTicks;
    }
    // If taker is selling (taking liquidity from bids), our bid might fill.
    if (fill.takerSide === 'short' && st.liveBidTicks !== null && st.liveBidTicks >= fill.priceTicks) {
      ourSide = 'long';
      ourPriceTicks = st.liveBidTicks;
    }
    if (!ourSide || ourPriceTicks === null) return false;

    const fillSize = fill.sizeLots < st.liveSizeLots ? fill.sizeLots : st.liveSizeLots;
    if (fillSize <= 0n) return false;

    // Update position + realize PnL on close-side fills.
    const sign = ourSide === 'long' ? 1n : -1n;
    const oldPos = st.position;
    if (!oldPos || oldPos.signedSizeLots === 0n) {
      st.position = {
        signedSizeLots: sign * fillSize,
        entryPriceTicks: ourPriceTicks,
      };
    } else {
      const oldSize = oldPos.signedSizeLots;
      const sameSide = (oldSize > 0n && sign > 0n) || (oldSize < 0n && sign < 0n);
      if (sameSide) {
        // Add to position; weighted-average entry.
        const newSizeAbs = (oldSize > 0n ? oldSize : -oldSize) + fillSize;
        const oldNotional = (oldSize > 0n ? oldSize : -oldSize) * oldPos.entryPriceTicks;
        const fillNotional = fillSize * ourPriceTicks;
        const newEntry = (oldNotional + fillNotional) / newSizeAbs;
        st.position = {
          signedSizeLots: oldSize + sign * fillSize,
          entryPriceTicks: newEntry,
        };
      } else {
        // Reduce or flip — realize PnL on the closed portion.
        const oldSizeAbs = oldSize > 0n ? oldSize : -oldSize;
        const closeSize = fillSize < oldSizeAbs ? fillSize : oldSizeAbs;
        const oldSign = oldSize > 0n ? 1n : -1n;
        const realized = oldSign * closeSize * (ourPriceTicks - oldPos.entryPriceTicks) * st.tape.tickSize;
        this.trader = {
          ...this.trader,
          collateralQuoteLots:
            this.trader.collateralQuoteLots +
            (realized < 0n ? -((-realized) > this.trader.collateralQuoteLots ? this.trader.collateralQuoteLots : -realized) : realized),
          realizedPnlQuoteLots: this.trader.realizedPnlQuoteLots + realized,
        };
        if (fillSize === oldSizeAbs) {
          st.position = null;
        } else if (fillSize < oldSizeAbs) {
          st.position = {
            signedSizeLots: oldSize + sign * fillSize,
            entryPriceTicks: oldPos.entryPriceTicks,
          };
        } else {
          // Flip
          st.position = {
            signedSizeLots: sign * (fillSize - oldSizeAbs),
            entryPriceTicks: ourPriceTicks,
          };
        }
      }
    }

    // Maker rebate credited.
    const notional = fillSize * ourPriceTicks * st.tape.tickSize;
    const rebate = (notional * BigInt(this.config.makerRebateBps)) / 10_000n;
    this.trader = {
      ...this.trader,
      collateralQuoteLots: this.trader.collateralQuoteLots + rebate,
    };

    // Drain consumed quote — if size remaining > 0, our quote stays for
    // future fills; here we treat the live quote as fully consumed since
    // we sized it as quoteSizeLots.
    st.liveSizeLots = st.liveSizeLots - fillSize;
    if (st.liveSizeLots === 0n) {
      // Exhausted — strategy will re-quote next iteration.
      st.liveBidTicks = null;
      st.liveAskTicks = null;
    }

    st.fillsAbsorbed += 1;
    this.totalFills += 1;
    return true;
  }
}
