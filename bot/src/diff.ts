// Quote diffing — decides whether a new quote is meaningfully different
// from what's already live on chain. If not, the bot skips the
// cancel-replace cycle and saves a tx fee. Net effect on calm markets:
// ~10x fewer tx than the legacy bot.
//
// Diff windows:
//   priceDiffBps — re-quote only if either side moves more than X bps.
//   sizeDiffBps  — re-quote only if size moves more than X bps.
//
// Setting both to 0 reproduces the legacy always-cancel-replace flow.

import type { LiveQuote, QuoteState } from './types.ts';

export interface DiffDecision {
  /// True if at least one side moved enough to warrant a re-quote.
  shouldRequote: boolean;
  /// True if either side flipped between non-zero and zero (new side
  /// appeared or disappeared) — always re-quotes on this regardless of
  /// the price/size thresholds.
  sideToggled: boolean;
  reason?: string | undefined;
}

export interface DiffInput {
  proposed: QuoteState;
  live: LiveQuote;
  priceDiffBps: number;
  sizeDiffBps: number;
}

/// Returns the absolute difference of `a` and `b` as a bigint magnitude.
function absDiff(a: bigint, b: bigint): bigint {
  return a > b ? a - b : b - a;
}

/// Returns true if `delta` exceeds `bps` of `reference`. Operates on
/// bigints; uses Number for the bps conversion (safe given bps ≤ 10_000).
function exceedsBpsThreshold(delta: bigint, reference: bigint, bps: number): boolean {
  if (reference === 0n) return delta > 0n;
  if (bps <= 0) return delta > 0n;
  // delta × 10_000 > reference × bps
  const lhs = delta * 10_000n;
  const rhs = reference * BigInt(bps);
  return lhs > rhs;
}

export function diffQuotes(input: DiffInput): DiffDecision {
  const { proposed, live, priceDiffBps, sizeDiffBps } = input;

  const proposedHasBid = proposed.bidTicks > 0n;
  const liveHasBid = live.bidTicks !== null && live.bidTicks > 0n;
  const proposedHasAsk = proposed.askTicks > 0n;
  const liveHasAsk = live.askTicks !== null && live.askTicks > 0n;

  // Side toggle: a side appeared or disappeared. Always re-quote.
  if (proposedHasBid !== liveHasBid || proposedHasAsk !== liveHasAsk) {
    return {
      shouldRequote: true,
      sideToggled: true,
      reason: `side toggled bid:${liveHasBid}->${proposedHasBid} ask:${liveHasAsk}->${proposedHasAsk}`,
    };
  }

  // Price diff on bid.
  if (proposedHasBid && live.bidTicks !== null) {
    const delta = absDiff(proposed.bidTicks, live.bidTicks);
    if (exceedsBpsThreshold(delta, live.bidTicks, priceDiffBps)) {
      return {
        shouldRequote: true,
        sideToggled: false,
        reason: `bid moved ${delta} ticks vs threshold ${priceDiffBps}bps of ${live.bidTicks}`,
      };
    }
  }

  // Price diff on ask.
  if (proposedHasAsk && live.askTicks !== null) {
    const delta = absDiff(proposed.askTicks, live.askTicks);
    if (exceedsBpsThreshold(delta, live.askTicks, priceDiffBps)) {
      return {
        shouldRequote: true,
        sideToggled: false,
        reason: `ask moved ${delta} ticks vs threshold ${priceDiffBps}bps of ${live.askTicks}`,
      };
    }
  }

  // Size diff.
  if (live.sizeLots > 0n) {
    const sizeDelta = absDiff(proposed.sizeLots, live.sizeLots);
    if (exceedsBpsThreshold(sizeDelta, live.sizeLots, sizeDiffBps)) {
      return {
        shouldRequote: true,
        sideToggled: false,
        reason: `size moved ${sizeDelta} vs threshold ${sizeDiffBps}bps of ${live.sizeLots}`,
      };
    }
  }

  return { shouldRequote: false, sideToggled: false };
}
