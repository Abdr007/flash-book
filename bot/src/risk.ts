// Risk gates — pure decision logic. No I/O.
//
// Three gate types compose:
//   • Drawdown kill switch (session-realized PnL ≤ floor → halt)
//   • Collateral floor (free collateral < min → no new quotes)
//   • Inventory cap (per side: skip the side that would breach the cap)

import type { RiskLimits } from './types.ts';

export interface RiskGateOutput {
  canQuote: boolean;
  skipBid: boolean;
  skipAsk: boolean;
  killSwitchActive: boolean;
  reason?: string | undefined;
}

export interface CheckRiskInput {
  inventorySignedLots: bigint;
  collateralQuoteLots: bigint;
  realizedPnlQuoteLots: bigint;
  limits: RiskLimits;
  quoteSizeLots: bigint;
}

export function checkRiskGates(args: CheckRiskInput): RiskGateOutput {
  const { limits } = args;

  if (args.realizedPnlQuoteLots <= limits.maxDrawdownQuoteLots) {
    return {
      canQuote: false,
      skipBid: true,
      skipAsk: true,
      killSwitchActive: true,
      reason: `kill switch: pnl ${args.realizedPnlQuoteLots} ≤ drawdown ${limits.maxDrawdownQuoteLots}`,
    };
  }

  if (args.collateralQuoteLots < limits.minCollateralQuoteLots) {
    return {
      canQuote: false,
      skipBid: true,
      skipAsk: true,
      killSwitchActive: false,
      reason: `collateral ${args.collateralQuoteLots} < floor ${limits.minCollateralQuoteLots}`,
    };
  }

  const projectedLong = args.inventorySignedLots + args.quoteSizeLots;
  const projectedShort = args.inventorySignedLots - args.quoteSizeLots;
  const skipBid = projectedLong > limits.maxInventoryLots;
  const skipAsk = -projectedShort > limits.maxInventoryLots;

  return {
    canQuote: !(skipBid && skipAsk),
    skipBid,
    skipAsk,
    killSwitchActive: false,
    reason: skipBid && skipAsk ? 'inventory cap hit on both sides' : undefined,
  };
}

/// Combine global + per-market risk limits, taking the more restrictive
/// value for each field. Per-market limits override global when set.
export function mergeRiskLimits(global: RiskLimits, perMarket?: RiskLimits): RiskLimits {
  if (!perMarket) return global;
  return {
    maxInventoryLots:
      perMarket.maxInventoryLots < global.maxInventoryLots
        ? perMarket.maxInventoryLots
        : global.maxInventoryLots,
    maxDrawdownQuoteLots:
      perMarket.maxDrawdownQuoteLots > global.maxDrawdownQuoteLots
        ? perMarket.maxDrawdownQuoteLots
        : global.maxDrawdownQuoteLots,
    minCollateralQuoteLots:
      perMarket.minCollateralQuoteLots > global.minCollateralQuoteLots
        ? perMarket.minCollateralQuoteLots
        : global.minCollateralQuoteLots,
  };
}
