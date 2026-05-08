// Off-chain risk preview — mirrors the on-chain `assess_margin` algorithm
// in `programs/flash-book/src/matcher/risk.rs`.
//
// Use this client-side to predict liquidation BEFORE submitting a trade,
// to render a "% to liquidation" indicator in a UI, or to drive a
// liquidation bot's "should I submit liquidate_position?" decision.
//
// Result is *advisory*. The on-chain matcher's stress lattice is the
// authoritative truth (it will reject a `liquidate_position` call if the
// trader is actually healthy).

import type { MarketAccount, PositionAccount } from './accounts.ts';

export interface StressScenario {
  readonly name: string;
  /** Map of market base58 → signed shock in bps. */
  readonly shocks: Map<string, number>;
}

export interface RiskPreview {
  readonly collateral: number;
  readonly unrealizedPnl: number;
  readonly equity: number;
  readonly required: number;
  readonly isHealthy: boolean;
  readonly worstScenario: string;
  /** Distance to liquidation as a fraction of equity. 1.0 = at threshold,
   *  > 1.0 = healthy buffer, ≤ 0 = already liquidatable. */
  readonly healthRatio: number;
}

/** Generate the default scenario lattice (mirrors `default_scenarios` in Rust). */
export function defaultScenarios(markets: ReadonlyArray<string>): StressScenario[] {
  const out: StressScenario[] = [{ name: 'flat', shocks: new Map() }];
  const grid = [-2000, -1000, -500, -200, 200, 500, 1000, 2000];
  for (const m of markets) {
    for (const shock of grid) {
      const sign = shock > 0 ? '+' : '';
      out.push({
        name: `${m}_${sign}${shock}bps`,
        shocks: new Map([[m, shock]]),
      });
    }
  }
  const allDown = new Map<string, number>();
  const allUp = new Map<string, number>();
  const bsDown = new Map<string, number>();
  const bsUp = new Map<string, number>();
  for (const m of markets) {
    allDown.set(m, -1000);
    allUp.set(m, 1000);
    bsDown.set(m, -3000);
    bsUp.set(m, 3000);
  }
  out.push({ name: 'all_down_10pct', shocks: allDown });
  out.push({ name: 'all_up_10pct', shocks: allUp });
  out.push({ name: 'black_swan_down', shocks: bsDown });
  out.push({ name: 'black_swan_up', shocks: bsUp });
  return out;
}

interface PositionLite {
  readonly market: string;
  readonly side: 'long' | 'short';
  readonly sizeLots: number;
  readonly entryPriceTicks: number;
}

interface MarketLite {
  readonly markPriceTicks: number;
  readonly tickSize: number;
  readonly maintenanceMarginRatioBps: number;
}

function toLite(positions: ReadonlyArray<PositionAccount>): PositionLite[] {
  const out: PositionLite[] = [];
  for (const p of positions) {
    if (p.sizeLots.isZero()) continue;
    out.push({
      market: p.market.toBase58(),
      side: p.side === 0 ? 'long' : 'short',
      sizeLots: p.sizeLots.toNumber(),
      entryPriceTicks: p.entryPriceTicks.toNumber(),
    });
  }
  return out;
}

function marketsLite(markets: ReadonlyMap<string, MarketAccount>): Map<string, MarketLite> {
  const out = new Map<string, MarketLite>();
  for (const [k, m] of markets) {
    out.set(k, {
      markPriceTicks: m.markPriceTicks.toNumber(),
      tickSize: m.params.tickSize.toNumber(),
      maintenanceMarginRatioBps: m.params.maintenanceMarginRatioBps,
    });
  }
  return out;
}

function unrealizedPnlQuoteLots(p: PositionLite, atPrice: number, tickSize: number): number {
  const sign = p.side === 'long' ? 1 : -1;
  return sign * p.sizeLots * (atPrice - p.entryPriceTicks) * tickSize;
}

/**
 * Preview a portfolio's risk against the configured stress lattice.
 *
 * `markets` keys must be the base58-encoded market PDA matching
 * `position.market.toBase58()`.
 */
export function previewPortfolioRisk(
  positions: ReadonlyArray<PositionAccount>,
  markets: ReadonlyMap<string, MarketAccount>,
  collateralQuoteLots: number,
  scenarios?: ReadonlyArray<StressScenario>,
): RiskPreview {
  const ps = toLite(positions);
  const ms = marketsLite(markets);
  const sc = scenarios ?? defaultScenarios([...ms.keys()]);

  // Equity at current marks.
  let unrealizedTotal = 0;
  for (const p of ps) {
    const m = ms.get(p.market);
    if (!m) continue;
    unrealizedTotal += unrealizedPnlQuoteLots(p, m.markPriceTicks, m.tickSize);
  }
  const equity = collateralQuoteLots + unrealizedTotal;

  // For each scenario, compute portfolio loss + maintenance margin.
  let worstLoss = 0;
  let worstName = 'flat';

  for (const scen of sc) {
    let loss = 0;
    for (const p of ps) {
      const m = ms.get(p.market);
      if (!m) continue;
      const shockBps = scen.shocks.get(p.market) ?? 0;
      const stressed = m.markPriceTicks * (1 + shockBps / 10_000);
      // Loss = -unrealized at stressed price (positive = bad).
      loss += -unrealizedPnlQuoteLots(p, stressed, m.tickSize);
      // Maintenance margin on stressed notional.
      loss += p.sizeLots * stressed * m.tickSize * (m.maintenanceMarginRatioBps / 10_000);
    }
    if (loss > worstLoss) {
      worstLoss = loss;
      worstName = scen.name;
    }
  }

  const required = worstLoss;
  const isHealthy = equity >= required;
  const healthRatio = required > 0 ? equity / required : Number.POSITIVE_INFINITY;

  return {
    collateral: collateralQuoteLots,
    unrealizedPnl: unrealizedTotal,
    equity,
    required,
    isHealthy,
    worstScenario: worstName,
    healthRatio,
  };
}

/**
 * Initial-margin requirement for a hypothetical new position.
 * Useful for "would this order get rejected?" checks before submission.
 */
export function initialMarginRequired(
  sizeLots: number,
  priceTicks: number,
  market: MarketAccount,
): number {
  return (
    sizeLots
    * priceTicks
    * market.params.tickSize.toNumber()
    * (market.params.initialMarginRatioBps / 10_000)
  );
}
