// Order simulator — pure-TS Walrasian clearing, mirrors
// `programs/flash-book/src/matcher/fba.rs::clear_batch`.
//
// Use this to preview what would happen if a hypothetical order joined
// the next batch. Returns clearing price, volume, and per-order fills.
// Pairs with `previewPortfolioRisk` for full pre-trade analysis:
//
//   1. Fetch the order buffer.
//   2. Add the prospective order.
//   3. simulateBatchClearing(orders, priorMark) → predicted fill.
//   4. If fill > 0: project new position state, run previewPortfolioRisk
//      against the post-fill portfolio.
//
// Result is *advisory*. The on-chain matcher's actual clear is the
// authoritative truth (it runs the same algorithm but also includes the
// FLP virtual quotes which are synthesized inside the program).

export type SimSide = 'long' | 'short';

/** Order priority. Lower number = higher priority in eligible-order sort. */
export const SIM_PRIORITY: Record<string, number> = {
  liquidation: 0,
  adl: 1,
  taker: 2,
  flp_virtual: 3,
  limit: 4,
};

export interface SimOrder {
  readonly id: string;
  readonly trader: string;
  readonly side: SimSide;
  readonly orderType: keyof typeof SIM_PRIORITY;
  readonly sizeLots: number;
  readonly limitTicks: number;
  readonly seq: number;
}

export interface SimFill {
  readonly takerId: string;
  readonly makerId: string;
  readonly takerTrader: string;
  readonly makerTrader: string;
  readonly takerSide: SimSide;
  readonly sizeLots: number;
  readonly priceTicks: number;
}

export interface SimResult {
  readonly clearingPriceTicks: number;
  readonly clearingVolumeLots: number;
  readonly fills: SimFill[];
}

function sumBuysAtOrAbove(buys: ReadonlyArray<SimOrder>, p: number): number {
  let total = 0;
  for (const o of buys) if (o.limitTicks >= p) total += o.sizeLots;
  return total;
}

function sumSellsAtOrBelow(sells: ReadonlyArray<SimOrder>, p: number): number {
  let total = 0;
  for (const o of sells) if (o.limitTicks <= p) total += o.sizeLots;
  return total;
}

function isTaker(o: SimOrder): boolean {
  return o.orderType === 'taker' || o.orderType === 'liquidation' || o.orderType === 'adl';
}

function fifoKey(o: SimOrder): [number, number] {
  return [SIM_PRIORITY[o.orderType] ?? 99, o.seq];
}

function compareFifo(a: SimOrder, b: SimOrder): number {
  const ka = fifoKey(a);
  const kb = fifoKey(b);
  if (ka[0] !== kb[0]) return ka[0] - kb[0];
  return ka[1] - kb[1];
}

/**
 * Walrasian uniform-price clearing. Mirrors the on-chain algorithm
 * exactly (modulo integer-vs-number arithmetic on the JS side).
 *
 * Tie-breaking when multiple prices yield max(min(D, S)):
 *   1. If the indifference interval contains `priorMarkTicks`, return that.
 *   2. Otherwise return the price closest to `priorMarkTicks`.
 */
export function simulateBatchClearing(
  orders: ReadonlyArray<SimOrder>,
  priorMarkTicks: number,
): SimResult {
  if (orders.length === 0) {
    return { clearingPriceTicks: priorMarkTicks, clearingVolumeLots: 0, fills: [] };
  }

  const buys = orders.filter((o) => o.side === 'long');
  const sells = orders.filter((o) => o.side === 'short');
  if (buys.length === 0 || sells.length === 0) {
    return { clearingPriceTicks: priorMarkTicks, clearingVolumeLots: 0, fills: [] };
  }

  // Candidate prices: union of limit prices, sorted, deduped.
  const set = new Set<number>();
  for (const o of orders) set.add(o.limitTicks);
  const sorted = [...set].sort((a, b) => a - b);

  // Find prices achieving max(min(D, S)).
  let bestVolume = 0;
  let bestPrices: number[] = [];
  for (const p of sorted) {
    const d = sumBuysAtOrAbove(buys, p);
    const s = sumSellsAtOrBelow(sells, p);
    const v = Math.min(d, s);
    if (v > bestVolume) {
      bestVolume = v;
      bestPrices = [p];
    } else if (v === bestVolume && v > 0) {
      bestPrices.push(p);
    }
  }

  if (bestVolume === 0 || bestPrices.length === 0) {
    return { clearingPriceTicks: priorMarkTicks, clearingVolumeLots: 0, fills: [] };
  }

  // Tie-break.
  let clearingPrice: number;
  if (bestPrices.length === 1) {
    clearingPrice = bestPrices[0]!;
  } else {
    const minP = Math.min(...bestPrices);
    const maxP = Math.max(...bestPrices);
    if (priorMarkTicks >= minP && priorMarkTicks <= maxP) {
      clearingPrice = priorMarkTicks;
    } else {
      clearingPrice = bestPrices.reduce((closest, p) =>
        Math.abs(p - priorMarkTicks) < Math.abs(closest - priorMarkTicks) ? p : closest,
      );
    }
  }

  // Fill in FIFO priority order.
  const eligibleBuys = buys.filter((o) => o.limitTicks >= clearingPrice).sort(compareFifo);
  const eligibleSells = sells.filter((o) => o.limitTicks <= clearingPrice).sort(compareFifo);

  const fills: SimFill[] = [];
  let buyIdx = 0;
  let sellIdx = 0;
  let buyRemaining = eligibleBuys[0]?.sizeLots ?? 0;
  let sellRemaining = eligibleSells[0]?.sizeLots ?? 0;

  while (buyIdx < eligibleBuys.length && sellIdx < eligibleSells.length) {
    const buy = eligibleBuys[buyIdx]!;
    const sell = eligibleSells[sellIdx]!;

    // Self-trade prevention.
    if (buy.trader === sell.trader) {
      const buyPrio = SIM_PRIORITY[buy.orderType] ?? 99;
      const sellPrio = SIM_PRIORITY[sell.orderType] ?? 99;
      if (buyPrio >= sellPrio) {
        buyIdx++;
        buyRemaining = eligibleBuys[buyIdx]?.sizeLots ?? 0;
      } else {
        sellIdx++;
        sellRemaining = eligibleSells[sellIdx]?.sizeLots ?? 0;
      }
      continue;
    }

    const fillSize = Math.min(buyRemaining, sellRemaining);
    if (fillSize <= 0) break;

    const buyTaker = isTaker(buy);
    const sellTaker = isTaker(sell);
    let taker: SimOrder, maker: SimOrder;
    if (buyTaker && !sellTaker) {
      taker = buy;
      maker = sell;
    } else if (sellTaker && !buyTaker) {
      taker = sell;
      maker = buy;
    } else {
      // Both takers or both makers — by priority.
      const buyPrio = SIM_PRIORITY[buy.orderType] ?? 99;
      const sellPrio = SIM_PRIORITY[sell.orderType] ?? 99;
      if (buyPrio <= sellPrio) {
        taker = buy;
        maker = sell;
      } else {
        taker = sell;
        maker = buy;
      }
    }

    fills.push({
      takerId: taker.id,
      makerId: maker.id,
      takerTrader: taker.trader,
      makerTrader: maker.trader,
      takerSide: taker.side,
      sizeLots: fillSize,
      priceTicks: clearingPrice,
    });

    buyRemaining -= fillSize;
    sellRemaining -= fillSize;
    if (buyRemaining <= 0) {
      buyIdx++;
      buyRemaining = eligibleBuys[buyIdx]?.sizeLots ?? 0;
    }
    if (sellRemaining <= 0) {
      sellIdx++;
      sellRemaining = eligibleSells[sellIdx]?.sizeLots ?? 0;
    }
  }

  return {
    clearingPriceTicks: clearingPrice,
    clearingVolumeLots: bestVolume,
    fills,
  };
}

/**
 * Compute the size + price filled for a specific order ID in the simulation
 * result. Returns `{ sizeLots: 0, priceTicks: 0 }` if the order didn't fill
 * (or filled zero size).
 */
export function fillForOrder(
  result: SimResult,
  orderId: string,
): { sizeLots: number; priceTicks: number } {
  let total = 0;
  let price = 0;
  for (const f of result.fills) {
    if (f.takerId === orderId || f.makerId === orderId) {
      total += f.sizeLots;
      price = f.priceTicks; // uniform across fills
    }
  }
  return { sizeLots: total, priceTicks: price };
}
