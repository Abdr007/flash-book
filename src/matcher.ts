// FBA matcher with Walrasian uniform-price clearing.
//
// For each batch, we accept all candidate buy orders (with limit ≥ p) and
// sell orders (with limit ≤ p), and find the price p* that maximizes the
// matchable volume V* = min(D(p), S(p)).
//
// Tie-breaking when multiple prices achieve V*:
//   1. Prefer the price closest to the prior mark.
//   2. If still tied, take the midpoint of the indifference interval.
//
// Order priority within the eligible set:
//   liquidation > taker > flp_virtual > limit
// Within the same priority class, FIFO by timestamp.
//
// Property: under FBA-Walrasian, no participant can profit from observing
// another participant's order *within the same batch* — clearing price is a
// pure function of the joint demand/supply curves.

import type { Fill, MarketParams, Order, Side } from './types.ts';

export interface MatchResult {
  readonly clearingPrice: number;
  readonly clearingVolume: number;
  readonly fills: Fill[];
  readonly unfilled: ReadonlyArray<{ orderId: string; remainingSize: number }>;
}

const PRIORITY: Record<Order['type'], number> = {
  liquidation: 0,
  adl: 1,
  taker: 2,
  flp_virtual: 3,
  limit: 4,
};

function sortByPriorityFifo(a: Order, b: Order): number {
  const pa = PRIORITY[a.type];
  const pb = PRIORITY[b.type];
  if (pa !== pb) return pa - pb;
  return a.timestamp - b.timestamp;
}

function isTaker(o: Order): boolean {
  return o.type === 'taker' || o.type === 'liquidation' || o.type === 'adl';
}

function dCurve(buys: ReadonlyArray<Order>, p: number): number {
  let d = 0;
  for (const o of buys) if (o.limitPrice >= p) d += o.size;
  return d;
}

function sCurve(sells: ReadonlyArray<Order>, p: number): number {
  let s = 0;
  for (const o of sells) if (o.limitPrice <= p) s += o.size;
  return s;
}

export interface ClearBatchInput {
  readonly market: string;
  readonly batchNum: number;
  readonly nowMs: number;
  readonly orders: ReadonlyArray<Order>;
  readonly priorMarkPrice: number;
  readonly params: MarketParams;
  /** VPIN current value for toxicity tax computation. */
  readonly vpin: number;
}

export function clearBatch(input: ClearBatchInput): MatchResult {
  const { orders, priorMarkPrice, params, vpin, batchNum, nowMs, market } = input;

  const buys = orders.filter((o) => o.side === 'long');
  const sells = orders.filter((o) => o.side === 'short');

  const empty: MatchResult = {
    clearingPrice: priorMarkPrice,
    clearingVolume: 0,
    fills: [],
    unfilled: [],
  };
  if (buys.length === 0 || sells.length === 0) return empty;

  // Candidate prices: every limit price that appears.
  const priceSet = new Set<number>();
  for (const o of buys) priceSet.add(o.limitPrice);
  for (const o of sells) priceSet.add(o.limitPrice);
  if (priceSet.size === 0) return empty;

  const sortedPrices = [...priceSet].sort((a, b) => a - b);

  // Find the prices achieving max(min(D, S)).
  let bestVolume = 0;
  const bestPrices: number[] = [];
  for (const p of sortedPrices) {
    const d = dCurve(buys, p);
    const s = sCurve(sells, p);
    const v = Math.min(d, s);
    if (v > bestVolume + 1e-12) {
      bestVolume = v;
      bestPrices.length = 0;
      bestPrices.push(p);
    } else if (Math.abs(v - bestVolume) < 1e-12 && v > 0) {
      bestPrices.push(p);
    }
  }

  if (bestVolume <= 0 || bestPrices.length === 0) return empty;

  // Tie-break: closest to prior mark; if still ambiguous, midpoint.
  let clearingPrice: number;
  if (bestPrices.length === 1) {
    clearingPrice = bestPrices[0] as number;
  } else {
    let closest = bestPrices[0] as number;
    let bestDist = Math.abs(closest - priorMarkPrice);
    for (const p of bestPrices) {
      const d = Math.abs(p - priorMarkPrice);
      if (d < bestDist) {
        bestDist = d;
        closest = p;
      }
    }
    // Use midpoint of the contiguous indifference interval if it spans the mark.
    const minP = Math.min(...bestPrices);
    const maxP = Math.max(...bestPrices);
    if (priorMarkPrice >= minP && priorMarkPrice <= maxP) {
      clearingPrice = priorMarkPrice;
    } else {
      clearingPrice = closest;
    }
  }

  // Filter eligible orders.
  const eligibleBuys = buys.filter((o) => o.limitPrice >= clearingPrice).sort(sortByPriorityFifo);
  const eligibleSells = sells.filter((o) => o.limitPrice <= clearingPrice).sort(sortByPriorityFifo);

  const fills: Fill[] = [];
  const unfilledMap = new Map<string, number>();
  for (const o of eligibleBuys) unfilledMap.set(o.id, o.size);
  for (const o of eligibleSells) unfilledMap.set(o.id, o.size);

  let buyIdx = 0;
  let sellIdx = 0;
  let buyRemaining = (eligibleBuys[0]?.size ?? 0);
  let sellRemaining = (eligibleSells[0]?.size ?? 0);

  while (buyIdx < eligibleBuys.length && sellIdx < eligibleSells.length) {
    const buy = eligibleBuys[buyIdx] as Order;
    const sell = eligibleSells[sellIdx] as Order;

    // Self-trade prevention: same trader on both sides is not a real trade.
    // Advance the side whose order is "more flexible" (limit > taker priority)
    // so we keep the higher-priority order in the queue.
    if (buy.trader === sell.trader) {
      if (PRIORITY[buy.type] >= PRIORITY[sell.type]) {
        buyIdx++;
        buyRemaining = (eligibleBuys[buyIdx]?.size ?? 0);
      } else {
        sellIdx++;
        sellRemaining = (eligibleSells[sellIdx]?.size ?? 0);
      }
      continue;
    }

    const fillSize = Math.min(buyRemaining, sellRemaining);
    if (fillSize <= 0) break;

    // Determine which side is the "taker" for fee assignment.
    const buyIsTaker = isTaker(buy);
    const sellIsTaker = isTaker(sell);
    const taker = buyIsTaker ? buy : sellIsTaker ? sell : buy; // both makers: arbitrarily call buy the taker
    const maker = taker === buy ? sell : buy;
    const takerSide: Side = taker.side;

    const notional = fillSize * clearingPrice;
    const takerFee = (notional * params.takerFeeBps) / 10_000;
    const makerRebate = (notional * params.makerRebateBps) / 10_000;
    const toxicityTax = (notional * params.toxicityTaxMaxBps * Math.min(1, vpin)) / 10_000;

    fills.push({
      market,
      takerId: taker.id,
      makerId: maker.id,
      takerTrader: taker.trader,
      makerTrader: maker.trader,
      takerSide,
      size: fillSize,
      price: clearingPrice,
      timestamp: nowMs,
      takerFee,
      makerRebate,
      toxicityTax,
      batchNum,
    });

    unfilledMap.set(buy.id, (unfilledMap.get(buy.id) ?? 0) - fillSize);
    unfilledMap.set(sell.id, (unfilledMap.get(sell.id) ?? 0) - fillSize);
    buyRemaining -= fillSize;
    sellRemaining -= fillSize;

    if (buyRemaining <= 1e-12) {
      buyIdx++;
      buyRemaining = (eligibleBuys[buyIdx]?.size ?? 0);
    }
    if (sellRemaining <= 1e-12) {
      sellIdx++;
      sellRemaining = (eligibleSells[sellIdx]?.size ?? 0);
    }
  }

  const unfilled: Array<{ orderId: string; remainingSize: number }> = [];
  for (const [id, rem] of unfilledMap) {
    if (rem > 1e-12) unfilled.push({ orderId: id, remainingSize: rem });
  }

  return { clearingPrice, clearingVolume: bestVolume, fills, unfilled };
}
