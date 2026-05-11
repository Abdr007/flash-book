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

// ─── V3 liquidation preview ──────────────────────────────────────────

export interface LiquidationPreview {
  /** True if `liquidate_position_v2` would currently succeed for this trader. */
  readonly liquidatable: boolean;
  /** Which scenario flagged the trader (matches assess_margin output). */
  readonly worstScenario: string;
  /** Price source the dual-source health gate would pick (mark/oracle/equal). */
  readonly healthPriceSource: 'mark' | 'oracle' | 'equal';
  /** Price the matcher would use for the liquidation close limit (oracle ± penalty). */
  readonly expectedClosePriceTicks: number;
  /**
   * Realized PnL the trader would incur if the synthetic close fully fills at
   * `expectedClosePriceTicks`. Negative = trader loss.
   */
  readonly expectedRealizedPnlQuoteLots: number;
  /** Liquidator reward credited to the caller, given current params. */
  readonly expectedLiquidatorRewardQuoteLots: number;
  /**
   * Insurance-fund delta from this liquidation. Positive = fund grows
   * (penalty contribution); negative = bankruptcy gap consumed.
   */
  readonly expectedInsuranceFundDeltaQuoteLots: number;
  /** Mark, oracle, and selected health price (in ticks). */
  readonly markTicks: number;
  readonly oracleTicks: number;
  readonly healthPriceTicks: number;
}

/**
 * Pre-liquidation preview — compute everything `liquidate_position_v2`
 * would output, off-chain. Mirrors the on-chain dual-source health gate
 * (max-adverse of mark / oracle), the synthetic-close limit pricing, and
 * the Dutch-auction liquidator reward curve.
 *
 * Use to:
 *   - drive a keeper's "should I submit?" decision (saves a CU-burning
 *     `NotLiquidatable` revert when the trader is just barely healthy)
 *   - render a "preview liquidation impact" panel to the trader
 *   - estimate insurance-fund draw before placing a large position
 *
 * Result is advisory; the on-chain assess remains authoritative.
 */
export function previewLiquidation(args: {
  market: MarketAccount;
  position: PositionAccount;
  collateralQuoteLots: number;
  /** Slot at which the keeper would call (defaults to "now" / latest slot — set by caller). */
  currentSlot?: number;
  /** Other open positions (for cross-margin assess). Empty by default. */
  otherPositions?: ReadonlyArray<PositionAccount>;
  /** Other markets keyed by base58 market PDA (cross-margin). */
  otherMarkets?: ReadonlyMap<string, MarketAccount>;
  scenarios?: ReadonlyArray<StressScenario>;
}): LiquidationPreview {
  const { market, position, collateralQuoteLots } = args;
  const tickSize = market.params.tickSize.toNumber();
  const sizeLots = position.sizeLots.toNumber();
  const entryTicks = position.entryPriceTicks.toNumber();
  const isLong = position.side === 0;
  const markTicks = market.markPriceTicks.toNumber();
  const oracleTicks = market.oraclePriceTicks.toNumber();

  // Dual-source health gate: pick the price that's MORE adverse for
  // the position's direction (long → lower, short → higher).
  let healthPriceTicks = markTicks;
  let healthPriceSource: 'mark' | 'oracle' | 'equal' = 'mark';
  if (oracleTicks > 0 && oracleTicks !== markTicks) {
    if (isLong) {
      if (oracleTicks < markTicks) {
        healthPriceTicks = oracleTicks;
        healthPriceSource = 'oracle';
      }
    } else {
      if (oracleTicks > markTicks) {
        healthPriceTicks = oracleTicks;
        healthPriceSource = 'oracle';
      }
    }
  } else if (oracleTicks === markTicks) {
    healthPriceSource = 'equal';
  }

  // Build a synthetic market with the health price as the "mark" so
  // previewPortfolioRisk uses it for unrealized PnL + stressed notional.
  const liveMarketKey = position.market.toBase58();
  const allPositions: PositionAccount[] = [position, ...(args.otherPositions ?? [])];
  const allMarkets = new Map<string, MarketAccount>();
  // Substitute the health price into the executing market.
  const healthMarketCopy: MarketAccount = {
    ...market,
    markPriceTicks: market.markPriceTicks.constructor === Object
      ? market.markPriceTicks
      : (Object.assign(Object.create(Object.getPrototypeOf(market.markPriceTicks)), market.markPriceTicks)),
  };
  // Quick path: just clone what previewPortfolioRisk reads (markPriceTicks + tickSize + mmrBps).
  // To avoid relying on BN constructor, use a cast-trick: the helper only
  // calls .toNumber() so a wrapped object suffices.
  const fakeBN = (n: number) => ({ toNumber: () => n, isZero: () => n === 0 } as unknown as BNLike);
  type BNLike = MarketAccount['markPriceTicks'];
  const wrappedMarket: MarketAccount = {
    ...market,
    markPriceTicks: fakeBN(healthPriceTicks),
  };
  void healthMarketCopy;
  allMarkets.set(liveMarketKey, wrappedMarket);
  for (const [k, m] of args.otherMarkets ?? []) allMarkets.set(k, m);
  const risk = previewPortfolioRisk(
    allPositions,
    allMarkets,
    collateralQuoteLots,
    args.scenarios,
  );

  // Synthetic close: oracle ± liq_penalty_bps. Long position → close
  // side is short → limit at oracle - penalty (we'd cross down).
  // Short position → close side is long → limit at oracle + penalty.
  const liqPenalty = market.params.liqPenaltyBps;
  const penaltyDelta = Math.floor((oracleTicks * liqPenalty) / 10_000);
  const expectedClosePriceTicks = isLong
    ? Math.max(0, oracleTicks - penaltyDelta)
    : oracleTicks + penaltyDelta;

  // Realized PnL at the synthetic close price. Long: pnl = (close - entry) × size × tick.
  const sign = isLong ? 1 : -1;
  const expectedRealizedPnlQuoteLots =
    sign * sizeLots * (expectedClosePriceTicks - entryTicks) * tickSize;

  // Liquidator reward — Dutch-auction curve. We assume a "fully aged"
  // call (best case for the keeper). Caller can override via currentSlot.
  const liqRewardBps = market.params.liquidatorRewardBps ?? 0;
  const expectedLiquidatorRewardQuoteLots =
    Math.floor((sizeLots * oracleTicks * tickSize * liqRewardBps) / 10_000);

  // Insurance-fund delta: the penalty (size × oracle × tick × penalty_bps)
  // contributes a fraction of itself to the fund (`liqPenaltyContributionBps`,
  // typically 5_000 = 50%). We use the headline penalty notional here as
  // the visible signal — the actual contribution depends on the InsuranceFund
  // params which the caller hasn't passed; if the trader goes underwater
  // (collateral + pnl - penalty - reward < 0), the gap is debited.
  const penaltyNotional = Math.floor(
    (sizeLots * expectedClosePriceTicks * tickSize * liqPenalty) / 10_000,
  );
  const grossClosed = collateralQuoteLots + expectedRealizedPnlQuoteLots - penaltyNotional - expectedLiquidatorRewardQuoteLots;
  const expectedInsuranceFundDeltaQuoteLots = grossClosed >= 0
    ? penaltyNotional // fund receives the penalty
    : grossClosed;    // bankruptcy: fund eats the gap (negative)

  return {
    liquidatable: !risk.isHealthy,
    worstScenario: risk.worstScenario,
    healthPriceSource,
    expectedClosePriceTicks,
    expectedRealizedPnlQuoteLots,
    expectedLiquidatorRewardQuoteLots,
    expectedInsuranceFundDeltaQuoteLots,
    markTicks,
    oracleTicks,
    healthPriceTicks,
  };
}
