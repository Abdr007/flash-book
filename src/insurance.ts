// Insurance fund — bankruptcy waterfall.
//
// Layer 1: collateral covers loss
// Layer 2: insurance fund covers shortfall
// Layer 3: ADL — most-profitable counter-positions are auto-deleveraged
//
// The October 2025 crash showed that under-funded insurance is the primary
// failure mode for perp DEXes ($5B liquidations overwhelmed funds → ADL).
// We design for a target balance ≥ 1% of OI, contributed by:
//   - 50% of liquidation penalties
//   - 50% of toxicity tax
//   - 10% of trading fees

import type { InsuranceFund, MarketState } from './types.ts';

export function createInsuranceFund(opts: {
  initialBalance: number;
  feeContributionRate: number;
  toxicityTaxContributionRate: number;
  liqPenaltyContributionRate: number;
  pauseNewPositionsBelow: number;
}): InsuranceFund {
  return {
    balance: opts.initialBalance,
    feeContributionRate: opts.feeContributionRate,
    toxicityTaxContributionRate: opts.toxicityTaxContributionRate,
    liqPenaltyContributionRate: opts.liqPenaltyContributionRate,
    totalContributions: 0,
    totalPayouts: 0,
    pauseNewPositionsBelow: opts.pauseNewPositionsBelow,
  };
}

export function contributeFromFees(fund: InsuranceFund, totalFees: number): number {
  if (totalFees <= 0) return 0;
  const c = totalFees * fund.feeContributionRate;
  fund.balance += c;
  fund.totalContributions += c;
  return c;
}

export function contributeFromToxicityTax(fund: InsuranceFund, totalTax: number): number {
  if (totalTax <= 0) return 0;
  const c = totalTax * fund.toxicityTaxContributionRate;
  fund.balance += c;
  fund.totalContributions += c;
  return c;
}

export function contributeFromLiqPenalty(fund: InsuranceFund, totalPenalty: number): number {
  if (totalPenalty <= 0) return 0;
  const c = totalPenalty * fund.liqPenaltyContributionRate;
  fund.balance += c;
  fund.totalContributions += c;
  return c;
}

export interface BankruptcyResolution {
  readonly covered: number;
  readonly remaining: number; // shortfall left after fund draw → ADL needed
}

export function coverShortfall(fund: InsuranceFund, shortfall: number): BankruptcyResolution {
  if (shortfall <= 0) return { covered: 0, remaining: 0 };
  const covered = Math.min(fund.balance, shortfall);
  fund.balance -= covered;
  fund.totalPayouts += covered;
  return { covered, remaining: shortfall - covered };
}

export function newPositionsAllowed(fund: InsuranceFund): boolean {
  return fund.balance >= fund.pauseNewPositionsBelow;
}

/** Recommended fund size: 1% of total OI notional across all markets. */
export function recommendedFundSize(markets: ReadonlyMap<string, MarketState>): number {
  let totalOiNotional = 0;
  for (const m of markets.values()) {
    totalOiNotional += (m.openInterestLong + m.openInterestShort) * m.markPrice;
  }
  return totalOiNotional * 0.01;
}
