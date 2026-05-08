import { describe, expect, test } from 'bun:test';
import {
  contributeFromFees,
  contributeFromLiqPenalty,
  contributeFromToxicityTax,
  coverShortfall,
  createInsuranceFund,
  newPositionsAllowed,
} from '../src/insurance.ts';

function makeFund(initial = 1000) {
  return createInsuranceFund({
    initialBalance: initial,
    feeContributionRate: 0.1,
    toxicityTaxContributionRate: 0.5,
    liqPenaltyContributionRate: 0.5,
    pauseNewPositionsBelow: 100,
  });
}

describe('insurance fund', () => {
  test('contribute from fees', () => {
    const f = makeFund(0);
    const c = contributeFromFees(f, 1000);
    expect(c).toBe(100);
    expect(f.balance).toBe(100);
    expect(f.totalContributions).toBe(100);
  });

  test('contribute from toxicity tax', () => {
    const f = makeFund(0);
    contributeFromToxicityTax(f, 200);
    expect(f.balance).toBe(100);
  });

  test('contribute from liq penalty', () => {
    const f = makeFund(0);
    contributeFromLiqPenalty(f, 200);
    expect(f.balance).toBe(100);
  });

  test('coverShortfall pays from balance', () => {
    const f = makeFund(500);
    const r = coverShortfall(f, 200);
    expect(r.covered).toBe(200);
    expect(r.remaining).toBe(0);
    expect(f.balance).toBe(300);
    expect(f.totalPayouts).toBe(200);
  });

  test('coverShortfall returns remaining when fund insufficient', () => {
    const f = makeFund(100);
    const r = coverShortfall(f, 500);
    expect(r.covered).toBe(100);
    expect(r.remaining).toBe(400);
    expect(f.balance).toBe(0);
  });

  test('newPositionsAllowed gates by threshold', () => {
    const f = makeFund(50);
    expect(newPositionsAllowed(f)).toBe(false);
    contributeFromFees(f, 1000);
    expect(newPositionsAllowed(f)).toBe(true);
  });
});
