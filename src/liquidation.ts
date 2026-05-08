// Liquidation engine — runs in-loop with the matcher every batch.
//
// Per batch:
//   1. assessMargin() across all traders
//   2. Inject LiquidationOrder for unhealthy traders into the same batch
//   3. Matcher clears them at the batch's uniform price (no race, no MEV)
//   4. If a liquidation closes at a price where collateral cannot cover loss,
//      the shortfall flows: insurance fund → ADL → socialized loss
//
// In-loop liquidations have three properties no other DEX achieves:
//   - Deterministic: same inputs → same liquidation price (no keeper race)
//   - Cascade-resilient: all liqs in a batch clear at the same uniform price
//     (in continuous matching, the 5th liq gets a much worse price than the 1st)
//   - No MEV extraction: no external party captures liq fees or front-runs

import { assessMargin } from './risk.ts';
import type {
  LiquidationEvent,
  MarginAssessment,
  MarketState,
  Order,
  Position,
  StressScenario,
} from './types.ts';

export interface LiquidationCandidate {
  readonly trader: string;
  readonly assessment: MarginAssessment;
  readonly positions: ReadonlyArray<Position>;
}

export function detectLiquidations(
  positionsByTrader: ReadonlyMap<string, ReadonlyArray<Position>>,
  collateralByTrader: ReadonlyMap<string, number>,
  markets: ReadonlyMap<string, MarketState>,
  scenarios: ReadonlyArray<StressScenario>,
): LiquidationCandidate[] {
  const result: LiquidationCandidate[] = [];
  for (const [trader, positions] of positionsByTrader) {
    if (positions.length === 0) continue;
    const collateral = collateralByTrader.get(trader) ?? 0;
    const assessment = assessMargin(positions, markets, scenarios, collateral);
    if (!assessment.isHealthy) {
      result.push({ trader, assessment, positions });
    }
  }
  return result;
}

export interface LiquidationOrderGenInput {
  readonly candidates: ReadonlyArray<LiquidationCandidate>;
  readonly markets: ReadonlyMap<string, MarketState>;
  readonly nowMs: number;
  readonly batchNum: number;
}

/**
 * Generate liquidation orders. Each candidate's positions become injected
 * orders on the opposite side at oracle ± liqPenalty (the worst price the
 * matcher will accept). The matcher will fill at the batch clearing price
 * which is at least as good as the limit.
 */
export function generateLiquidationOrders(input: LiquidationOrderGenInput): Order[] {
  const orders: Order[] = [];
  for (const cand of input.candidates) {
    for (const pos of cand.positions) {
      const m = input.markets.get(pos.market);
      if (!m || pos.size <= 0) continue;
      const penalty = m.params.liqPenaltyBps / 10_000;
      const closeSide = pos.side === 'long' ? 'short' : 'long';
      const limitPrice =
        closeSide === 'short'
          ? m.oraclePrice * (1 - penalty)
          : m.oraclePrice * (1 + penalty);
      orders.push({
        id: `liq_${cand.trader}_${pos.market}_b${input.batchNum}`,
        market: pos.market,
        trader: cand.trader,
        side: closeSide,
        size: pos.size,
        limitPrice,
        type: 'liquidation',
        timestamp: input.nowMs,
        postOnly: false,
      });
    }
  }
  return orders;
}

/**
 * Compute the bankruptcy shortfall for a liquidation that filled at price `fillPrice`.
 * Returns positive shortfall (insurance fund / ADL needed) or 0 if collateral covers it.
 */
export function computeShortfall(
  position: Position,
  fillPrice: number,
  market: MarketState,
): { liquidationPenalty: number; shortfall: number; collateralRecovered: number } {
  const sign = position.side === 'long' ? 1 : -1;
  const realizedPnl = sign * position.size * (fillPrice - position.entryPrice);
  const liquidationPenalty =
    (position.size * fillPrice * market.params.liqPenaltyBps) / 10_000;
  const remaining = position.collateral + realizedPnl - liquidationPenalty;
  if (remaining >= 0) {
    return {
      liquidationPenalty,
      shortfall: 0,
      collateralRecovered: remaining,
    };
  }
  return {
    liquidationPenalty,
    shortfall: -remaining,
    collateralRecovered: 0,
  };
}

export function makeLiquidationEvent(args: {
  position: Position;
  fillPrice: number;
  market: MarketState;
  insuranceFundContribution: number;
  bankruptShortfall: number;
  collateralRecovered: number;
  batchNum: number;
}): LiquidationEvent {
  return {
    trader: args.position.trader,
    market: args.position.market,
    side: args.position.side,
    size: args.position.size,
    liquidationPrice: args.fillPrice,
    collateralRecovered: args.collateralRecovered,
    insuranceFundContribution: args.insuranceFundContribution,
    bankruptShortfall: args.bankruptShortfall,
    batchNum: args.batchNum,
  };
}
