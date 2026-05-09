import { describe, expect, test } from 'bun:test';
import { PublicKey } from '@solana/web3.js';
import {
  IcebergOrder,
  OcoOrder,
  TrailingStopOrder,
} from '../src/order-types.ts';

const M = new PublicKey('11111111111111111111111111111112');

describe('OcoOrder', () => {
  test('first tick emits two place actions (TP + SL)', () => {
    const oco = new OcoOrder({
      market: M,
      side: 'short',
      sizeLots: 1n,
      takeProfitTicks: 105_000n,
      stopLossTicks: 95_000n,
    });
    const acts = oco.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    expect(acts.length).toBe(2);
    expect(acts.every((a) => a.type === 'place')).toBe(true);
  });

  test('TP fill cancels SL', () => {
    const oco = new OcoOrder({
      market: M,
      side: 'short',
      sizeLots: 1n,
      takeProfitTicks: 105_000n,
      stopLossTicks: 95_000n,
    });
    oco.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    oco.bind([
      { seq: 1n, limitTicks: 105_000n }, // TP
      { seq: 2n, limitTicks: 95_000n },  // SL
    ]);
    const acts = oco.tick({
      markPriceTicks: 105_000n,
      activeSeqs: [{ seq: 2n, sizeLots: 1n, limitTicks: 95_000n, side: 'short' }],
      newFills: [{ seq: 1n, sizeLots: 1n }],
    });
    expect(acts.length).toBe(1);
    expect(acts[0]!.type).toBe('cancel');
    if (acts[0]!.type === 'cancel') expect(acts[0]!.seq).toBe(2n);
    expect(oco.state.done).toBe(true);
  });

  test('done=true emits noop on subsequent ticks', () => {
    const oco = new OcoOrder({
      market: M,
      side: 'short',
      sizeLots: 1n,
      takeProfitTicks: 105_000n,
      stopLossTicks: 95_000n,
    });
    oco.state.done = true;
    const acts = oco.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    expect(acts.length).toBe(1);
    expect(acts[0]!.type).toBe('noop');
  });
});

describe('IcebergOrder', () => {
  test('first tick places one slice', () => {
    const ice = new IcebergOrder({
      market: M,
      side: 'long',
      totalSizeLots: 100n,
      visibleSizeLots: 10n,
      limitTicks: 100_000n,
    });
    const acts = ice.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    expect(acts.length).toBe(1);
    expect(acts[0]!.type).toBe('place');
    if (acts[0]!.type === 'place') {
      expect(acts[0]!.sizeLots).toBe(10n);
    }
  });

  test('after a slice fill, places the next slice', () => {
    const ice = new IcebergOrder({
      market: M,
      side: 'long',
      totalSizeLots: 100n,
      visibleSizeLots: 10n,
      limitTicks: 100_000n,
    });
    ice.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    ice.bindSliceSeq(7n);
    const acts = ice.tick({
      markPriceTicks: 100_000n,
      activeSeqs: [],
      newFills: [{ seq: 7n, sizeLots: 10n }],
    });
    expect(acts.length).toBe(1);
    expect(acts[0]!.type).toBe('place');
    expect(ice.state.filledLots).toBe(10n);
  });

  test('totalSize reached → emits noop', () => {
    const ice = new IcebergOrder({
      market: M,
      side: 'long',
      totalSizeLots: 10n,
      visibleSizeLots: 5n,
      limitTicks: 100_000n,
    });
    ice.state.filledLots = 10n;
    const acts = ice.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    expect(acts[0]!.type).toBe('noop');
  });

  test('partial slice on the last fill', () => {
    const ice = new IcebergOrder({
      market: M,
      side: 'long',
      totalSizeLots: 12n,
      visibleSizeLots: 10n,
      limitTicks: 100_000n,
    });
    ice.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    ice.bindSliceSeq(7n);
    ice.tick({
      markPriceTicks: 100_000n,
      activeSeqs: [],
      newFills: [{ seq: 7n, sizeLots: 10n }],
    });
    // Now 10 filled, 2 remaining → next slice = 2.
    ice.bindSliceSeq(8n);
    expect(ice.state.filledLots).toBe(10n);
    const acts = ice.tick({
      markPriceTicks: 100_000n,
      activeSeqs: [{ seq: 8n, sizeLots: 2n, limitTicks: 100_000n, side: 'long' }],
      newFills: [],
    });
    expect(acts[0]!.type).toBe('noop');
  });
});

describe('TrailingStopOrder', () => {
  test('short trailing stop tracks high mark, triggers on drop', () => {
    const ts = new TrailingStopOrder({
      market: M,
      side: 'short',
      sizeLots: 1n,
      trailBps: 100, // 1% trail
    });
    // Mark rises 100k → 110k → trail would be 1100 below = 108_900.
    ts.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    ts.tick({ markPriceTicks: 110_000n, activeSeqs: [], newFills: [] });
    expect(ts.state.bestMarkTicks).toBe(110_000n);
    expect(ts.state.currentStopTicks).toBe(108_900n);
    expect(ts.state.triggered).toBe(false);
    // Mark drops to 108_500 — below stop → trigger.
    const acts = ts.tick({ markPriceTicks: 108_500n, activeSeqs: [], newFills: [] });
    expect(ts.state.triggered).toBe(true);
    expect(acts.length).toBe(1);
    expect(acts[0]!.type).toBe('place');
  });

  test('long trailing stop tracks low mark, triggers on rise', () => {
    const ts = new TrailingStopOrder({
      market: M,
      side: 'long',
      sizeLots: 1n,
      trailBps: 100,
    });
    ts.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    ts.tick({ markPriceTicks: 90_000n, activeSeqs: [], newFills: [] });
    expect(ts.state.bestMarkTicks).toBe(90_000n);
    // Stop = 90_000 + 1% = 90_900.
    expect(ts.state.currentStopTicks).toBe(90_900n);
    const acts = ts.tick({ markPriceTicks: 91_000n, activeSeqs: [], newFills: [] });
    expect(ts.state.triggered).toBe(true);
    expect(acts[0]!.type).toBe('place');
  });

  test('does not retrigger after first execution', () => {
    const ts = new TrailingStopOrder({
      market: M,
      side: 'short',
      sizeLots: 1n,
      trailBps: 100,
    });
    ts.state.executedSeq = 5n;
    const acts = ts.tick({ markPriceTicks: 100_000n, activeSeqs: [], newFills: [] });
    expect(acts[0]!.type).toBe('noop');
  });
});
