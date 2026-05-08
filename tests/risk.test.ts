import { describe, expect, test } from 'bun:test';
import { assessMargin, generateScenarios } from '../src/risk.ts';
import { makeTestMarket as makeMarket, makeTestPosition as pos } from './_helpers.ts';

describe('stress-lattice margin', () => {
  test('healthy long with adequate collateral', () => {
    const m = new Map([['SOL', makeMarket('SOL', 100)]]);
    const positions = [pos('SOL', 'long', 1, 100)];
    const scenarios = generateScenarios(['SOL']);
    const a = assessMargin(positions, m, scenarios, 50);
    expect(a.isHealthy).toBe(true);
  });

  test('unhealthy when collateral insufficient for worst-case scenario', () => {
    const m = new Map([['SOL', makeMarket('SOL', 100)]]);
    const positions = [pos('SOL', 'long', 10, 100)];
    const scenarios = generateScenarios(['SOL']);
    const a = assessMargin(positions, m, scenarios, 5);
    expect(a.isHealthy).toBe(false);
    expect(a.worstScenario).not.toBe('flat');
  });

  test('hedge recognition: long+short same market = much lower required margin than unhedged', () => {
    const market = new Map([['SOL', makeMarket('SOL', 100)]]);
    const scenarios = generateScenarios(['SOL']);

    // Unhedged: single 5-long.
    const unhedgedReq = assessMargin(
      [pos('SOL', 'long', 5, 100, 'T')],
      market,
      scenarios,
      0,
    ).required;

    // Hedged: long+short cancels directional risk, leaving only maintenance margin
    // on the stressed notional of both legs.
    const hedgedReq = assessMargin(
      [pos('SOL', 'long', 5, 100, 'T'), pos('SOL', 'short', 5, 100, 'T')],
      market,
      scenarios,
      0,
    ).required;

    // Hedge should reduce required margin by at least 5x.
    expect(hedgedReq).toBeLessThan(unhedgedReq / 5);
  });

  test('worst scenario name returned', () => {
    const m = new Map([['SOL', makeMarket('SOL', 100)]]);
    const positions = [pos('SOL', 'long', 10, 100)];
    const scenarios = generateScenarios(['SOL']);
    const a = assessMargin(positions, m, scenarios, 1);
    expect(a.worstScenario).not.toBe('flat');
  });
});

describe('generateScenarios', () => {
  test('includes flat, per-market shocks, and correlated shocks', () => {
    const scenarios = generateScenarios(['SOL', 'BTC']);
    const names = new Set(scenarios.map((s) => s.name));
    expect(names.has('flat')).toBe(true);
    expect(names.has('all_down_10pct')).toBe(true);
    expect(names.has('all_up_10pct')).toBe(true);
    expect(names.has('black_swan_down')).toBe(true);
    expect([...names].some((n) => n.startsWith('SOL_'))).toBe(true);
    expect([...names].some((n) => n.startsWith('BTC_'))).toBe(true);
  });
});
