import { describe, expect, test } from 'bun:test';
import { diffQuotes } from '../src/diff.ts';

describe('diffQuotes', () => {
  test('always re-quotes when no live quote exists', () => {
    const r = diffQuotes({
      proposed: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: null, askTicks: null, sizeLots: 0n },
      priceDiffBps: 5,
      sizeDiffBps: 0,
    });
    expect(r.shouldRequote).toBe(true);
    expect(r.sideToggled).toBe(true);
  });

  test('skips re-quote when prices unchanged inside diff window', () => {
    const r = diffQuotes({
      proposed: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 5,
      sizeDiffBps: 0,
    });
    expect(r.shouldRequote).toBe(false);
  });

  test('re-quotes when bid moves > priceDiffBps', () => {
    // Live bid 99_950, new bid 100_000 → delta = 50, 50 / 99_950 ≈ 5 bps.
    // priceDiffBps = 4 → exceeds threshold → re-quote.
    const r = diffQuotes({
      proposed: { bidTicks: 100_000n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 4,
      sizeDiffBps: 0,
    });
    expect(r.shouldRequote).toBe(true);
  });

  test('skips re-quote when bid moves < priceDiffBps', () => {
    // Live bid 99_950, new bid 99_960 → delta = 10, ≈ 1 bps.
    // priceDiffBps = 5 → within threshold → skip.
    const r = diffQuotes({
      proposed: { bidTicks: 99_960n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 5,
      sizeDiffBps: 0,
    });
    expect(r.shouldRequote).toBe(false);
  });

  test('re-quotes when bid side toggles off', () => {
    const r = diffQuotes({
      proposed: { bidTicks: 0n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 100,
      sizeDiffBps: 100,
    });
    expect(r.shouldRequote).toBe(true);
    expect(r.sideToggled).toBe(true);
  });

  test('re-quotes when size moves more than sizeDiffBps', () => {
    // Live size 1, proposed size 10 → 900 bps move.
    const r = diffQuotes({
      proposed: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 10n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 0,
      sizeDiffBps: 100, // 1%
    });
    expect(r.shouldRequote).toBe(true);
  });

  test('priceDiffBps = 0 always re-quotes on any move', () => {
    const r = diffQuotes({
      proposed: { bidTicks: 99_951n, askTicks: 100_050n, sizeLots: 1n },
      live: { bidTicks: 99_950n, askTicks: 100_050n, sizeLots: 1n },
      priceDiffBps: 0,
      sizeDiffBps: 0,
    });
    expect(r.shouldRequote).toBe(true);
  });
});
