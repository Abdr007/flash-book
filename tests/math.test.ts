import { describe, expect, test } from 'bun:test';
import { clamp, commitHash, emaUpdate, oracleBand, Prng, pushAndTwap, roundLot, roundToTick, safeNumber } from '../src/math.ts';

describe('clamp', () => {
  test('returns lo when x < lo', () => expect(clamp(-1, 0, 10)).toBe(0));
  test('returns hi when x > hi', () => expect(clamp(11, 0, 10)).toBe(10));
  test('returns x when in range', () => expect(clamp(5, 0, 10)).toBe(5));
});

describe('safeNumber', () => {
  test('returns x for finite', () => expect(safeNumber(3.14)).toBe(3.14));
  test('returns fallback for NaN', () => expect(safeNumber(NaN, -1)).toBe(-1));
  test('returns fallback for Infinity', () => expect(safeNumber(Infinity, 0)).toBe(0));
});

describe('roundToTick', () => {
  test('rounds to nearest tick', () => expect(roundToTick(100.123, 0.01)).toBeCloseTo(100.12));
  test('zero tick is identity', () => expect(roundToTick(100.5, 0)).toBe(100.5));
});

describe('roundLot', () => {
  test('floors to lot', () => expect(roundLot(0.123, 0.01)).toBeCloseTo(0.12));
  test('zero lot is identity', () => expect(roundLot(0.5, 0)).toBe(0.5));
});

describe('Prng', () => {
  test('deterministic from same seed', () => {
    const a = new Prng(42);
    const b = new Prng(42);
    for (let i = 0; i < 100; i++) expect(a.next()).toBe(b.next());
  });
  test('range bounds', () => {
    const r = new Prng(1);
    for (let i = 0; i < 1000; i++) {
      const v = r.range(10, 20);
      expect(v).toBeGreaterThanOrEqual(10);
      expect(v).toBeLessThan(20);
    }
  });
  test('normal distribution mean ≈ 0', () => {
    const r = new Prng(7);
    let sum = 0;
    const n = 10_000;
    for (let i = 0; i < n; i++) sum += r.normal();
    expect(Math.abs(sum / n)).toBeLessThan(0.05);
  });
});

describe('emaUpdate', () => {
  test('window 1 returns sample', () => expect(emaUpdate(10, 20, 1)).toBe(20));
  test('alpha math', () => {
    const out = emaUpdate(0, 1, 9);
    expect(out).toBeCloseTo(0.2);
  });
});

describe('pushAndTwap', () => {
  test('respects max length', () => {
    const buf: number[] = [];
    pushAndTwap(buf, 1, 3);
    pushAndTwap(buf, 2, 3);
    pushAndTwap(buf, 3, 3);
    pushAndTwap(buf, 4, 3);
    expect(buf).toEqual([2, 3, 4]);
  });
  test('returns mean', () => {
    const buf: number[] = [];
    pushAndTwap(buf, 10, 5);
    pushAndTwap(buf, 20, 5);
    expect(pushAndTwap(buf, 30, 5)).toBeCloseTo(20);
  });
});

describe('oracleBand', () => {
  test('clamps within band', () => {
    expect(oracleBand(102, 100, 100)).toBeCloseTo(101); // band = 100bps = 1%, max 101
    expect(oracleBand(99, 100, 100)).toBe(99);
    expect(oracleBand(105, 100, 200)).toBeCloseTo(102);
  });
});

describe('commitHash', () => {
  test('deterministic', () => {
    const h1 = commitHash(['SOL', 'long', 1, 100, 'nonce']);
    const h2 = commitHash(['SOL', 'long', 1, 100, 'nonce']);
    expect(h1).toBe(h2);
  });
  test('different inputs yield different hashes', () => {
    const h1 = commitHash(['SOL', 'long', 1, 100, 'nonce']);
    const h2 = commitHash(['SOL', 'long', 1, 100, 'nonce2']);
    expect(h1).not.toBe(h2);
  });
});
