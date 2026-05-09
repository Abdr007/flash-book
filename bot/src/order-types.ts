// Advanced order types — OCO, Iceberg, Trailing stop. All implemented
// off-chain on top of the base place_limit_order / cancel_order
// primitives. The on-chain matcher is unchanged: these are state
// machines the bot runs against the venue.
//
// References:
//   • OCO (One-Cancels-Other) — Binance, OKX, Coinbase. Two limit
//     orders posted; when one fills, the other is cancelled by the bot.
//     Standard hedge / SL+TP construction.
//   • Iceberg (also "reserve" orders) — Binance, FIX gateways. Show a
//     small visible slice; refill from a hidden reserve as it fills.
//     Hides large orders to reduce market-impact bias.
//   • Trailing stop — every CEX. Stop price tracks the market until
//     reversal triggers conversion to a market order.
//
// All three are state machines driven by fill events. The owning bot
// passes them in via `tick(observation)`; they emit OrderActions for
// the bot to apply against the venue.

import type { PublicKey } from '@solana/web3.js';

// ─── Common types ────────────────────────────────────────────────────

export type Side = 'long' | 'short';

/// What an order-type state machine emits per tick.
export type OrderTypeAction =
  | { type: 'place'; market: PublicKey; side: Side; sizeLots: bigint; limitTicks: bigint }
  | { type: 'cancel'; market: PublicKey; seq: bigint }
  | { type: 'noop' };

/// Observation passed to a state machine each tick.
export interface OrderTypeObservation {
  /// Current mark price in ticks for the market.
  markPriceTicks: bigint;
  /// Active order seqs the bot owns on this market (filtered to this
  /// state machine's orders by seq match).
  activeSeqs: ReadonlyArray<{ seq: bigint; sizeLots: bigint; limitTicks: bigint; side: Side }>;
  /// Fill events observed since the last tick. Caller filters by their
  /// own bookkeeping; we just react.
  newFills: ReadonlyArray<{ seq: bigint; sizeLots: bigint }>;
}

// ─── OCO ─────────────────────────────────────────────────────────────

export interface OcoConfig {
  market: PublicKey;
  side: Side;
  sizeLots: bigint;
  /// Take-profit limit price in ticks.
  takeProfitTicks: bigint;
  /// Stop-loss limit price in ticks (the bot converts to a market-style
  /// limit at this price when triggered).
  stopLossTicks: bigint;
}

export interface OcoState {
  /// Sequence of the take-profit order (set after first place).
  tpSeq?: bigint;
  /// Sequence of the stop-loss order.
  slSeq?: bigint;
  /// Set when one of the two has filled and the other should cancel.
  done: boolean;
}

export class OcoOrder {
  state: OcoState;

  constructor(public readonly config: OcoConfig) {
    this.state = { done: false };
  }

  /// Drive the state machine. Emits up to N actions per call.
  tick(obs: OrderTypeObservation): OrderTypeAction[] {
    if (this.state.done) return [{ type: 'noop' }];

    const actions: OrderTypeAction[] = [];

    // First-time placement — issue both legs.
    if (this.state.tpSeq === undefined && this.state.slSeq === undefined) {
      actions.push({
        type: 'place',
        market: this.config.market,
        side: this.config.side,
        sizeLots: this.config.sizeLots,
        limitTicks: this.config.takeProfitTicks,
      });
      actions.push({
        type: 'place',
        market: this.config.market,
        side: this.config.side,
        sizeLots: this.config.sizeLots,
        limitTicks: this.config.stopLossTicks,
      });
      // Caller is responsible for matching new seqs to the legs by
      // limit price after the place tx confirms.
      return actions;
    }

    // Detect fill on either leg.
    for (const fill of obs.newFills) {
      if (fill.seq === this.state.tpSeq) {
        // TP filled → cancel SL.
        if (this.state.slSeq !== undefined) {
          actions.push({ type: 'cancel', market: this.config.market, seq: this.state.slSeq });
        }
        this.state.done = true;
        return actions;
      }
      if (fill.seq === this.state.slSeq) {
        // SL filled → cancel TP.
        if (this.state.tpSeq !== undefined) {
          actions.push({ type: 'cancel', market: this.config.market, seq: this.state.tpSeq });
        }
        this.state.done = true;
        return actions;
      }
    }

    return [{ type: 'noop' }];
  }

  /// Caller informs the state machine of the seqs assigned to its legs
  /// after the initial place tx confirms. Match by limit price.
  bind(seqs: ReadonlyArray<{ seq: bigint; limitTicks: bigint }>): void {
    for (const s of seqs) {
      if (s.limitTicks === this.config.takeProfitTicks) this.state.tpSeq = s.seq;
      else if (s.limitTicks === this.config.stopLossTicks) this.state.slSeq = s.seq;
    }
  }
}

// ─── Iceberg ─────────────────────────────────────────────────────────

export interface IcebergConfig {
  market: PublicKey;
  side: Side;
  /// Total size to fill across all slices.
  totalSizeLots: bigint;
  /// Visible slice size — the bot only ever shows this much at a time.
  visibleSizeLots: bigint;
  limitTicks: bigint;
}

export interface IcebergState {
  filledLots: bigint;
  currentSeq?: bigint | undefined;
}

export class IcebergOrder {
  state: IcebergState = { filledLots: 0n };

  constructor(public readonly config: IcebergConfig) {}

  tick(obs: OrderTypeObservation): OrderTypeAction[] {
    // Account for fills since last tick.
    for (const fill of obs.newFills) {
      if (fill.seq === this.state.currentSeq) {
        this.state.filledLots += fill.sizeLots;
        this.state.currentSeq = undefined; // slice consumed
      }
    }

    if (this.state.filledLots >= this.config.totalSizeLots) {
      return [{ type: 'noop' }];
    }

    // Compute remaining + next slice size.
    const remaining = this.config.totalSizeLots - this.state.filledLots;
    const sliceSize =
      remaining < this.config.visibleSizeLots ? remaining : this.config.visibleSizeLots;

    // If we don't have an active slice, place one.
    if (this.state.currentSeq === undefined) {
      return [
        {
          type: 'place',
          market: this.config.market,
          side: this.config.side,
          sizeLots: sliceSize,
          limitTicks: this.config.limitTicks,
        },
      ];
    }

    // Slice still resting — verify it's still on the book at the right
    // size. If the visible size has been partially consumed (rare since
    // we treat fills as full slice consumption), top up.
    const live = obs.activeSeqs.find((a) => a.seq === this.state.currentSeq);
    if (!live) {
      // Got cancelled out-of-band — drop ref and re-place next tick.
      this.state.currentSeq = undefined;
    }

    return [{ type: 'noop' }];
  }

  bindSliceSeq(seq: bigint): void {
    this.state.currentSeq = seq;
  }
}

// ─── Trailing stop ───────────────────────────────────────────────────

export interface TrailingStopConfig {
  market: PublicKey;
  /// Direction of the trailing stop. 'long' = trailing-stop-buy
  /// (triggered when mark rises above stop). 'short' = trailing-stop-sell
  /// (triggered when mark falls below stop).
  side: Side;
  sizeLots: bigint;
  /// Distance the stop trails behind the best mark, in bps.
  trailBps: number;
}

export interface TrailingStopState {
  /// Best mark observed so far (highest for short-side trailing stops,
  /// lowest for long-side).
  bestMarkTicks?: bigint;
  /// Current stop price (derived from bestMark + trailBps direction).
  currentStopTicks?: bigint;
  /// Once triggered, switch to "execute" mode → place a limit at the
  /// stop price (effectively a market order at our trigger).
  triggered: boolean;
  executedSeq?: bigint;
}

export class TrailingStopOrder {
  state: TrailingStopState = { triggered: false };

  constructor(public readonly config: TrailingStopConfig) {}

  tick(obs: OrderTypeObservation): OrderTypeAction[] {
    if (this.state.executedSeq !== undefined) return [{ type: 'noop' }];

    const mark = obs.markPriceTicks;
    if (mark <= 0n) return [{ type: 'noop' }];

    // Update best mark + recompute stop.
    if (this.state.bestMarkTicks === undefined) {
      this.state.bestMarkTicks = mark;
    } else if (this.config.side === 'short') {
      // For a trailing-stop-sell, best mark = highest seen.
      if (mark > this.state.bestMarkTicks) this.state.bestMarkTicks = mark;
    } else {
      // For a trailing-stop-buy, best mark = lowest seen.
      if (mark < this.state.bestMarkTicks) this.state.bestMarkTicks = mark;
    }

    const trail = (this.state.bestMarkTicks * BigInt(this.config.trailBps)) / 10_000n;
    this.state.currentStopTicks =
      this.config.side === 'short'
        ? this.state.bestMarkTicks - trail
        : this.state.bestMarkTicks + trail;

    // Trigger condition.
    const triggered =
      this.config.side === 'short'
        ? mark <= this.state.currentStopTicks
        : mark >= this.state.currentStopTicks;

    if (triggered && !this.state.triggered) {
      this.state.triggered = true;
      return [
        {
          type: 'place',
          market: this.config.market,
          side: this.config.side,
          sizeLots: this.config.sizeLots,
          limitTicks: this.state.currentStopTicks,
        },
      ];
    }

    return [{ type: 'noop' }];
  }

  bindExecution(seq: bigint): void {
    this.state.executedSeq = seq;
  }
}
