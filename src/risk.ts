// Stress-lattice maintenance margin.
//
// For a portfolio of positions {P_i}, define a finite scenario set S
// (per-asset shocks ±2/5/10/20%, plus correlated shocks). Required margin is:
//
//   M_maint = max_{s ∈ S} [ Σ_i loss_i(s) ]
//
// A trader is healthy iff M_maint ≤ collateral + Σ_i unrealized_pnl_i.
//
// This naturally recognizes hedges: a long SOL + short SOL position has zero
// directional risk in every shock, so M_maint ≈ 0 regardless of size.

import { safeNumber } from './math.ts';
import type {
  MarginAssessment,
  MarketState,
  Position,
  Side,
  StressScenario,
} from './types.ts';
import { fundingOwed } from './funding.ts';

/** Generate a default stress lattice for a set of markets. */
export function generateScenarios(markets: ReadonlyArray<string>): StressScenario[] {
  const scenarios: StressScenario[] = [{ name: 'flat', shocks: new Map() }];

  // Per-market single-asset shocks.
  const shockGrid = [-0.20, -0.10, -0.05, -0.02, 0.02, 0.05, 0.10, 0.20];
  for (const m of markets) {
    for (const shock of shockGrid) {
      const sign = shock > 0 ? '+' : '';
      scenarios.push({
        name: `${m}_${sign}${(shock * 100).toFixed(0)}pct`,
        shocks: new Map([[m, shock]]),
      });
    }
  }

  // Correlated stress: all-down / all-up.
  const allDown = new Map<string, number>();
  const allUp = new Map<string, number>();
  for (const m of markets) {
    allDown.set(m, -0.10);
    allUp.set(m, 0.10);
  }
  scenarios.push({ name: 'all_down_10pct', shocks: allDown });
  scenarios.push({ name: 'all_up_10pct', shocks: allUp });

  // Black-swan correlated.
  const blackSwanDown = new Map<string, number>();
  const blackSwanUp = new Map<string, number>();
  for (const m of markets) {
    blackSwanDown.set(m, -0.30);
    blackSwanUp.set(m, 0.30);
  }
  scenarios.push({ name: 'black_swan_down', shocks: blackSwanDown });
  scenarios.push({ name: 'black_swan_up', shocks: blackSwanUp });

  return scenarios;
}

function unrealizedPnl(position: Position, price: number): number {
  const sign = position.side === 'long' ? 1 : -1;
  return sign * position.size * (price - position.entryPrice);
}

/** Assess a trader's margin health given current markets and a scenario set. */
export function assessMargin(
  positions: ReadonlyArray<Position>,
  markets: ReadonlyMap<string, MarketState>,
  scenarios: ReadonlyArray<StressScenario>,
  collateral: number,
): MarginAssessment {
  let unrealizedTotal = 0;
  let fundingDueTotal = 0;
  for (const pos of positions) {
    const m = markets.get(pos.market);
    if (!m) continue;
    unrealizedTotal += safeNumber(unrealizedPnl(pos, m.markPrice));
    fundingDueTotal += safeNumber(fundingOwed(pos, m));
  }

  const equity = collateral + unrealizedTotal - fundingDueTotal;

  let worstLoss = 0;
  let worstScenario = 'flat';

  for (const scenario of scenarios) {
    let scenarioLoss = 0;
    for (const pos of positions) {
      const m = markets.get(pos.market);
      if (!m) continue;
      const shock = scenario.shocks.get(pos.market) ?? 0;
      const stressedPrice = m.markPrice * (1 + shock);
      // Loss = -unrealized at stressed price (positive number = bad)
      scenarioLoss += -unrealizedPnl(pos, stressedPrice);
      // Maintenance margin requirement on the stressed notional.
      scenarioLoss += pos.size * stressedPrice * m.params.maintenanceMarginRatio;
    }
    if (scenarioLoss > worstLoss) {
      worstLoss = scenarioLoss;
      worstScenario = scenario.name;
    }
  }

  const required = worstLoss;
  return {
    required,
    collateral,
    equity,
    isHealthy: equity >= required,
    worstScenario,
    worstLoss,
  };
}

/** Initial margin check used when opening a new position. */
export function initialMarginRequired(
  side: Side,
  size: number,
  price: number,
  market: MarketState,
): number {
  const _ = side;
  return size * price * market.params.initialMarginRatio;
}
