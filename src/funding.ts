// Continuous funding accrual via cumulative index.
//
// Every ER block, we advance a per-market cumulative funding index by:
//   ΔI = clamp(K · premium(t), ±r_max) · Δt
// where premium(t) = (mark - oracle) / oracle.
//
// On every position change, the position is charged:
//   Δ_charge = sign(side_long) · notional · (I_now - I_at_entry)
// Long pays when premium > 0 (mark > oracle), short pays when premium < 0.
//
// This eliminates funding sniping (the trade where you flip just before/after
// a discrete funding tick). Per-block resolution makes "holding 7h59m vs 8h01m"
// fair within ER block precision (~10ms).

import { clamp } from './math.ts';
import type { MarketState, Position } from './types.ts';

export interface FundingTick {
  readonly market: string;
  readonly newIndex: number;
  readonly indexDelta: number;
  readonly rate: number;
  readonly premium: number;
}

export function advanceFundingIndex(market: MarketState, blockDeltaMs: number): FundingTick {
  if (blockDeltaMs <= 0 || market.oraclePrice <= 0) {
    return {
      market: market.symbol,
      newIndex: market.cumFundingIndex,
      indexDelta: 0,
      rate: 0,
      premium: 0,
    };
  }
  const premium = (market.markPrice - market.oraclePrice) / market.oraclePrice;
  const rate = clamp(
    market.params.fundingRateK * premium,
    -market.params.fundingRateMaxPerSec,
    market.params.fundingRateMaxPerSec,
  );
  const dt = blockDeltaMs / 1000;
  const indexDelta = rate * dt;
  market.cumFundingIndex += indexDelta;
  market.lastFundingRate = rate;
  return {
    market: market.symbol,
    newIndex: market.cumFundingIndex,
    indexDelta,
    rate,
    premium,
  };
}

/**
 * Funding owed by a position since last settlement.
 * Long pays when the index has increased; short pays when it has decreased.
 * Returns positive = trader owes (debit).
 */
export function fundingOwed(position: Position, market: MarketState): number {
  const indexDelta = market.cumFundingIndex - position.cumFundingIndexAtEntry;
  const sign = position.side === 'long' ? 1 : -1;
  const notional = position.size * market.markPrice;
  return sign * notional * indexDelta;
}

/**
 * Settle funding into a position: charge owed amount against collateral,
 * track cumulative funding paid, and reset the index marker.
 */
export function settleFunding(position: Position, market: MarketState): number {
  const owed = fundingOwed(position, market);
  position.collateral -= owed;
  position.fundingPaid += owed;
  position.cumFundingIndexAtEntry = market.cumFundingIndex;
  return owed;
}
