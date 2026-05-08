// Cross-language parity test.
//
// Reads tests/parity/scenarios.json from the repo root, runs each
// scenario through the TS simulator (`simulateBatchClearing`), and asserts
// the computed clearing price/volume matches the documented expected
// outputs.
//
// The Rust program has a *parallel* test (`programs/flash-book/tests/parity_test.rs`)
// that reads the SAME json file and runs each scenario through the Rust
// matcher (`clear_batch`). Both must agree.
//
// If the two implementations ever drift apart, both tests fail loudly.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  fillForOrder,
  simulateBatchClearing,
  type SimOrder,
} from '../src/order-simulator.ts';

interface RawOrder {
  id: number;
  trader_seed: number;
  side: 'long' | 'short';
  order_type: 'limit' | 'taker' | 'flp_virtual' | 'liquidation' | 'adl';
  size: number;
  limit: number;
  seq: number;
}

interface Expected {
  clearing_price?: number;
  clearing_volume: number;
  fill_count?: number;
  first_fill_taker_seed?: number;
}

interface Scenario {
  name: string;
  orders: RawOrder[];
  prior_mark: number;
  expected: Expected;
}

interface FixtureFile {
  scenarios: Scenario[];
}

function loadFixtures(): FixtureFile {
  const path = join(__dirname, '..', '..', 'tests', 'parity', 'scenarios.json');
  return JSON.parse(readFileSync(path, 'utf8')) as FixtureFile;
}

function raw_to_sim(raw: RawOrder): SimOrder {
  return {
    id: `id_${raw.id}`,
    trader: `seed_${raw.trader_seed}`,
    side: raw.side,
    orderType: raw.order_type,
    sizeLots: raw.size,
    limitTicks: raw.limit,
    seq: raw.seq,
  };
}

const file = loadFixtures();

describe('Cross-language parity (TS simulator vs Rust matcher)', () => {
  for (const scenario of file.scenarios) {
    test(scenario.name, () => {
      const orders = scenario.orders.map(raw_to_sim);
      const r = simulateBatchClearing(orders, scenario.prior_mark);

      // Volume must match exactly.
      expect(r.clearingVolumeLots).toBe(scenario.expected.clearing_volume);

      // Clearing price.
      if (scenario.expected.clearing_price !== undefined) {
        expect(r.clearingPriceTicks).toBe(scenario.expected.clearing_price);
      }

      // Fill count.
      if (scenario.expected.fill_count !== undefined) {
        expect(r.fills.length).toBe(scenario.expected.fill_count);
      }

      // First-fill taker identity (by trader_seed).
      if (scenario.expected.first_fill_taker_seed !== undefined) {
        expect(r.fills.length).toBeGreaterThan(0);
        expect(r.fills[0]!.takerTrader).toBe(
          `seed_${scenario.expected.first_fill_taker_seed}`,
        );
      }
    });
  }

  test('all scenarios are non-empty', () => {
    expect(file.scenarios.length).toBeGreaterThan(0);
  });
});

// fillForOrder agrees with fills array — long-side total == short-side total
// (every fill is one buyer + one seller). The self-trade scenario produces
// clearing_volume > 0 with zero actual fills, which is correct (Walrasian max
// is theoretical; STP filters at fill time). So we compare against actual
// total fill size, not clearing_volume.
describe('fillForOrder agrees with simulator', () => {
  test('long-side total == short-side total == total fill size', () => {
    for (const scenario of file.scenarios) {
      const orders = scenario.orders.map(raw_to_sim);
      const r = simulateBatchClearing(orders, scenario.prior_mark);

      let longTotal = 0;
      let shortTotal = 0;
      for (const o of orders) {
        const fill = fillForOrder(r, o.id);
        if (o.side === 'long') longTotal += fill.sizeLots;
        else shortTotal += fill.sizeLots;
      }
      const totalFillSize = r.fills.reduce((sum, f) => sum + f.sizeLots, 0);
      expect(longTotal).toBe(totalFillSize);
      expect(shortTotal).toBe(totalFillSize);
    }
  });
});
