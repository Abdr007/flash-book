// Synthetic flow simulation — runs a 60-second Flash Book session with
// a mix of market makers, retail takers, and an oracle price walk.
//
// Demonstrates:
//   - Frequent batch auction clearing every 50ms
//   - Virtual FLP quoter participating in every batch
//   - MMs competing inside the FLP spread for profit
//   - VPIN evolving with order flow toxicity
//   - Mark price stability via TWAP + oracle band
//   - Continuous funding accrual
//   - Insurance fund growth from fees + toxicity tax
//
// Run:  bun run examples/synthetic-flow.ts

import {
  DEFAULT_MAJOR_MARKET_PARAMS,
  FlashBookEngine,
  Prng,
  type EngineConfig,
  type Side,
} from '../src/index.ts';

const SEED = 0xDEADBEEF;
const SESSION_MS = 60_000;
const BATCH_MS = 50;
const N_TRADERS = 20;
const N_MMS = 5;
const STARTING_CAPITAL = 50_000;
const FLP_CAPITAL = 5_000_000;

const config: EngineConfig = {
  scenarios: [],
  insuranceFund: {
    initialBalance: 50_000,
    feeContributionRate: 0.10,
    toxicityTaxContributionRate: 0.50,
    liqPenaltyContributionRate: 0.50,
    pauseNewPositionsBelow: 5_000,
  },
  commitRevealEnabled: false,
  commitExpiryBatches: 5,
  commitBondLamports: 1000,
};

const engine = new FlashBookEngine(config);
engine.addMarket({
  symbol: 'SOL',
  initialOraclePrice: 100,
  initialFlpCapital: FLP_CAPITAL,
  params: DEFAULT_MAJOR_MARKET_PARAMS,
});

// Seed traders & MMs.
const rng = new Prng(SEED);
const traders: string[] = [];
for (let i = 0; i < N_TRADERS; i++) {
  const id = `trader_${i}`;
  traders.push(id);
  engine.deposit(id, STARTING_CAPITAL);
}
const mms: string[] = [];
for (let i = 0; i < N_MMS; i++) {
  const id = `mm_${i}`;
  mms.push(id);
  engine.deposit(id, STARTING_CAPITAL * 5);
}

// Random walk for oracle.
let oracle = 100;
const ORACLE_VOL = 0.0008; // ~0.08% per batch step

interface RunStats {
  batches: number;
  totalFills: number;
  totalVolume: number;
  flpFills: number;
  mmFills: number;
  retailFills: number;
  liquidations: number;
  adlEvents: number;
  insuranceFundFinal: number;
  insuranceFundDelta: number;
  vpinFinal: number;
  markPriceFinal: number;
  oracleFinal: number;
  flpRealizedPnl: number;
  flpNetUsdFinal: number;
}

function randomTakerOrder(t: string): { side: Side; size: number; limit: number } | null {
  // 70% balanced, 30% directional cluster (toxic flow)
  const directional = rng.bool(0.3);
  const side: Side = directional ? 'long' : (rng.bool(0.5) ? 'long' : 'short');
  const size = Math.max(0.01, rng.range(0.1, 1.5));
  const slippageBps = rng.range(20, 80);
  const limit = side === 'long'
    ? oracle * (1 + slippageBps / 10_000)
    : oracle * (1 - slippageBps / 10_000);
  return { side, size, limit };
}

function mmQuote(): Array<{ side: Side; size: number; limit: number }> {
  // Each MM places a tight ask + tight bid.
  const widthBps = rng.range(2, 6);
  return [
    { side: 'long', size: rng.range(1, 3), limit: oracle * (1 - widthBps / 10_000) },
    { side: 'short', size: rng.range(1, 3), limit: oracle * (1 + widthBps / 10_000) },
  ];
}

const stats: RunStats = {
  batches: 0,
  totalFills: 0,
  totalVolume: 0,
  flpFills: 0,
  mmFills: 0,
  retailFills: 0,
  liquidations: 0,
  adlEvents: 0,
  insuranceFundFinal: 0,
  insuranceFundDelta: 0,
  vpinFinal: 0,
  markPriceFinal: 100,
  oracleFinal: 100,
  flpRealizedPnl: 0,
  flpNetUsdFinal: 0,
};

const initialInsurance = engine.insuranceFundView().balance;

console.log('═'.repeat(72));
console.log('  Flash Book — Synthetic Flow Simulation');
console.log('═'.repeat(72));
console.log(`  Session: ${SESSION_MS / 1000}s, batch interval: ${BATCH_MS}ms`);
console.log(`  Traders: ${N_TRADERS}, MMs: ${N_MMS}`);
console.log(`  FLP capital: $${FLP_CAPITAL.toLocaleString()}`);
console.log(`  Initial insurance: $${initialInsurance.toLocaleString()}`);
console.log('─'.repeat(72));

const startMs = Date.now();
let now = 0;
let nextSnapshot = 5_000;

while (now < SESSION_MS) {
  // Oracle drift.
  oracle = oracle * (1 + rng.normal(0, ORACLE_VOL));
  oracle = Math.max(1, oracle);
  engine.updateOraclePrice('SOL', oracle);

  // Each MM quotes both sides this batch (postOnly to never cross).
  for (const mm of mms) {
    for (const q of mmQuote()) {
      try {
        engine.submitLimitOrder({
          trader: mm,
          market: 'SOL',
          side: q.side,
          size: q.size,
          limitPrice: q.limit,
          postOnly: true,
        });
      } catch (_) {
        /* noop — out of margin etc. */
      }
    }
  }

  // Random subset of traders submit takers.
  for (const t of traders) {
    if (!rng.bool(0.10)) continue;
    const o = randomTakerOrder(t);
    if (!o) continue;
    try {
      engine.submitTakerOrder({
        trader: t,
        market: 'SOL',
        side: o.side,
        size: o.size,
        limitPrice: o.limit,
      });
    } catch (_) {
      /* margin or paused */
    }
  }

  // Run the batch.
  const result = engine.runBatch(now);
  stats.batches += 1;
  for (const m of result.perMarket.values()) {
    stats.totalFills += m.fills.length;
    for (const fill of m.fills) {
      stats.totalVolume += fill.size * fill.price;
      const isFlp = fill.makerTrader === 'FLP_POOL' || fill.takerTrader === 'FLP_POOL';
      const isMm = fill.makerTrader.startsWith('mm_') || fill.takerTrader.startsWith('mm_');
      const isRetail =
        fill.makerTrader.startsWith('trader_') || fill.takerTrader.startsWith('trader_');
      if (isFlp) stats.flpFills += 1;
      if (isMm) stats.mmFills += 1;
      if (isRetail) stats.retailFills += 1;
    }
  }
  stats.liquidations += result.liquidations.length;
  stats.adlEvents += result.adl.length;
  stats.insuranceFundDelta += result.insuranceFundDelta;

  if (now >= nextSnapshot) {
    const ms = engine.marketState('SOL');
    const ifv = engine.insuranceFundView();
    console.log(
      `  t=${(now / 1000).toFixed(1)}s | ` +
        `mark=${ms.markPrice.toFixed(2)} oracle=${ms.oraclePrice.toFixed(2)} ` +
        `vpin=${(ms.vpin * 100).toFixed(1)}% ` +
        `fills=${stats.totalFills} fund=$${ifv.balance.toFixed(0)}`,
    );
    nextSnapshot += 5_000;
  }

  now += BATCH_MS;
}

const ms = engine.marketState('SOL');
const ifv = engine.insuranceFundView();
const flp = engine.flpStateView();
stats.insuranceFundFinal = ifv.balance;
stats.vpinFinal = ms.vpin;
stats.markPriceFinal = ms.markPrice;
stats.oracleFinal = ms.oraclePrice;
stats.flpRealizedPnl = flp.realizedPnl;
stats.flpNetUsdFinal = engine.flpNetUsdAcrossMarkets();

const elapsed = Date.now() - startMs;

console.log('─'.repeat(72));
console.log('  RESULTS');
console.log('─'.repeat(72));
console.log(`  Batches run:          ${stats.batches.toLocaleString()}`);
console.log(`  Wall-clock:           ${elapsed}ms (${(stats.batches * 1000 / elapsed).toFixed(0)} batches/sec)`);
console.log(`  Total fills:          ${stats.totalFills.toLocaleString()}`);
console.log(`  Total volume:         $${stats.totalVolume.toLocaleString(undefined, { maximumFractionDigits: 0 })}`);
console.log(`  Avg fill size USD:    $${(stats.totalVolume / Math.max(stats.totalFills, 1)).toFixed(0)}`);
console.log('');
console.log(`  Fills involving FLP:  ${stats.flpFills} (${(100 * stats.flpFills / Math.max(stats.totalFills, 1)).toFixed(1)}%)`);
console.log(`  Fills involving MMs:  ${stats.mmFills} (${(100 * stats.mmFills / Math.max(stats.totalFills, 1)).toFixed(1)}%)`);
console.log(`  Fills involving ret:  ${stats.retailFills} (${(100 * stats.retailFills / Math.max(stats.totalFills, 1)).toFixed(1)}%)`);
console.log('');
console.log(`  Liquidations:         ${stats.liquidations}`);
console.log(`  ADL events:           ${stats.adlEvents}`);
console.log('');
console.log(`  Final mark:           $${stats.markPriceFinal.toFixed(4)}`);
console.log(`  Final oracle:         $${stats.oracleFinal.toFixed(4)}`);
console.log(`  Mark-oracle diff bps: ${((stats.markPriceFinal - stats.oracleFinal) / stats.oracleFinal * 10000).toFixed(2)}`);
console.log(`  Final VPIN:           ${(stats.vpinFinal * 100).toFixed(1)}%`);
console.log('');
console.log(`  Insurance fund:       $${stats.insuranceFundFinal.toLocaleString(undefined, { maximumFractionDigits: 0 })}`);
console.log(`  Fund growth:          $${(stats.insuranceFundFinal - initialInsurance).toFixed(0)}`);
console.log('');
console.log(`  FLP realized PnL:     $${stats.flpRealizedPnl.toFixed(2)}`);
console.log(`  FLP net exposure:     $${stats.flpNetUsdFinal.toFixed(2)}`);
console.log(`  FLP capital:          $${flp.totalCapital.toLocaleString(undefined, { maximumFractionDigits: 0 })}`);
console.log('═'.repeat(72));
