// Virtual FLP Quoter — synthesizes the FLP pool's two-sided quote ladder.
//
// This is the load-bearing innovation: the FLP pool participates in its own
// order book as a permanent maker-of-last-resort. Human MMs compete *inside*
// the pool's spread for profit; flow that MMs decline falls back to the pool
// at the same price retail used to get under the current oracle-priced model.
//
// Design lineage (best-in-class market-making theory applied):
//
//   • Avellaneda-Stoikov (2008) inventory-aware optimal market making:
//       reservation_price = mid − q · γ · σ² · (T − t)
//       optimal_spread    = γ · σ² · (T − t) + (2/γ)·ln(1 + γ/k)
//     We adapt this to a per-batch FBA setting: σ² is realized variance of
//     recent clearing prices, q is signed pool inventory as fraction of
//     capital, γ is configurable risk aversion.
//
//   • Glosten-Milgrom (1985) adverse selection: spread widens when informed
//     flow is detected. We use VPIN (Easley-LdP-O'Hara 2012) as the toxicity
//     signal feeding the α coefficient.
//
//   • Almgren-Chriss execution: depth amortization (κ · Q / depth_floor)
//     widens spread for larger sizes — the pool charges more for absorbing
//     bigger flows.
//
// Spread function (per level, for cumulative size Q):
//
//   s(Q) = s0_bps/1e4 + α·VPIN + β·u + γ·|oi_imb| + κ·(Q / depth_floor) + δ·σ_realized
//
// Inventory-aware mid:
//
//   skew      = −(λ + γ_risk · σ²) · (pool_net_usd / pool_capital)
//   fair_value = oracle · (1 + skew)
//
// When pool is net-short (q < 0), skew > 0 → fair_value above oracle →
// pool's bid is more attractive → pool buys back its short exposure first.

import type { MarketState, Order } from './types.ts';
import { FLP_TRADER_ID } from './types.ts';
import { roundToTick } from './math.ts';

export interface FlpQuoterInput {
  readonly market: MarketState;
  readonly poolCapitalUsd: number;
  readonly poolNetUsd: number;            // signed: + = long, − = short
  readonly poolGrossUtilization: number;  // |all FLP positions USD| / capital
  readonly nowMs: number;
  readonly batchNum: number;
}

export interface FlpQuoterOutput {
  readonly orders: Order[];
  readonly perLevelSize: number;
  readonly bidLadder: ReadonlyArray<{ price: number; size: number }>;
  readonly askLadder: ReadonlyArray<{ price: number; size: number }>;
  readonly skew: number;
  readonly fairValue: number;
  readonly realizedVol: number;
  readonly effectiveSpread: number;
}

/** Compute realized volatility (stdev of relative returns) from recent prices. */
function realizedVolatility(prices: ReadonlyArray<number>): number {
  if (prices.length < 2) return 0;
  const returns: number[] = [];
  for (let i = 1; i < prices.length; i++) {
    const prev = prices[i - 1] as number;
    const cur = prices[i] as number;
    if (prev > 0) returns.push((cur - prev) / prev);
  }
  if (returns.length === 0) return 0;
  let mean = 0;
  for (const r of returns) mean += r;
  mean /= returns.length;
  let varSum = 0;
  for (const r of returns) varSum += (r - mean) * (r - mean);
  const variance = varSum / returns.length;
  return Math.sqrt(variance);
}

export function generateFlpQuotes(input: FlpQuoterInput): FlpQuoterOutput {
  const { market, poolCapitalUsd, poolNetUsd, poolGrossUtilization, nowMs, batchNum } = input;
  const { params } = market;
  const empty: FlpQuoterOutput = {
    orders: [],
    perLevelSize: 0,
    bidLadder: [],
    askLadder: [],
    skew: 0,
    fairValue: market.oraclePrice,
    realizedVol: 0,
    effectiveSpread: 0,
  };

  if (poolCapitalUsd <= 0 || market.oraclePrice <= 0) return empty;

  const sigma = realizedVolatility(market.recentClearingPrices);
  const sigmaSq = sigma * sigma;

  // Inventory skew (Avellaneda-Stoikov-inspired, scaled by pool capital).
  const inventoryFraction = poolNetUsd / poolCapitalUsd;
  const skewMagnitude = params.flpInventoryLambda + params.flpRiskAversion * sigmaSq;
  const skew = -skewMagnitude * inventoryFraction;
  const fairValue = market.oraclePrice * (1 + skew);

  // OI imbalance — separate from inventory skew, used for spread widening only.
  const oiTotal = market.openInterestLong + market.openInterestShort;
  const oiImbalance = oiTotal > 0
    ? (market.openInterestLong - market.openInterestShort) / oiTotal
    : 0;

  // Per-batch growth cap split across N levels.
  const usdCapPerBatch = poolCapitalUsd * params.flpMaxGrowthPerBatchPct;
  const perLevelUsd = usdCapPerBatch / params.flpQuoteLevels;
  const perLevelSize = perLevelUsd / market.oraclePrice;

  if (perLevelSize <= 0 || params.flpQuoteLevels <= 0) return empty;

  const orders: Order[] = [];
  const bidLadder: Array<{ price: number; size: number }> = [];
  const askLadder: Array<{ price: number; size: number }> = [];
  let totalSpread = 0;

  for (let i = 1; i <= params.flpQuoteLevels; i++) {
    const cumSize = perLevelSize * i;
    const sBase = params.flpSpreadBaseBps / 10_000;
    const s =
      sBase +
      params.flpSpreadAlpha * market.vpin +
      params.flpSpreadBeta * poolGrossUtilization +
      params.flpSpreadGamma * Math.abs(oiImbalance) +
      params.flpSpreadKappa * (cumSize / Math.max(params.flpDepthFloor, 1)) +
      params.flpSpreadDelta * sigma;

    totalSpread += s;
    const bidPrice = roundToTick(fairValue * (1 - s), params.tickSize);
    const askPrice = roundToTick(fairValue * (1 + s), params.tickSize);

    if (bidPrice > 0) {
      orders.push({
        id: `flp_bid_b${batchNum}_l${i}`,
        market: market.symbol,
        trader: FLP_TRADER_ID,
        side: 'long',
        size: perLevelSize,
        limitPrice: bidPrice,
        type: 'flp_virtual',
        timestamp: nowMs,
        postOnly: false,
      });
      bidLadder.push({ price: bidPrice, size: perLevelSize });
    }

    if (askPrice > 0) {
      orders.push({
        id: `flp_ask_b${batchNum}_l${i}`,
        market: market.symbol,
        trader: FLP_TRADER_ID,
        side: 'short',
        size: perLevelSize,
        limitPrice: askPrice,
        type: 'flp_virtual',
        timestamp: nowMs,
        postOnly: false,
      });
      askLadder.push({ price: askPrice, size: perLevelSize });
    }
  }

  return {
    orders,
    perLevelSize,
    bidLadder,
    askLadder,
    skew,
    fairValue,
    realizedVol: sigma,
    effectiveSpread: totalSpread / Math.max(params.flpQuoteLevels, 1),
  };
}
