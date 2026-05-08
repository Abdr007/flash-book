import { describe, expect, test } from 'bun:test';
import { VpinCalculator } from '../src/vpin.ts';

describe('VpinCalculator', () => {
  test('zero before any buckets close', () => {
    const v = new VpinCalculator(100, 10);
    v.recordFill('long', 50);
    expect(v.value).toBe(0);
  });

  test('balanced flow → low VPIN', () => {
    const v = new VpinCalculator(100, 5);
    for (let i = 0; i < 50; i++) {
      v.recordFill('long', 10);
      v.recordFill('short', 10);
    }
    expect(v.value).toBeLessThan(0.2);
  });

  test('one-sided flow → high VPIN', () => {
    const v = new VpinCalculator(100, 5);
    for (let i = 0; i < 50; i++) {
      v.recordFill('long', 100);
    }
    expect(v.value).toBeGreaterThan(0.8);
  });

  test('snapshot exposes state', () => {
    const v = new VpinCalculator(100, 5);
    v.recordFill('long', 30);
    v.recordFill('short', 20);
    const s = v.snapshot();
    expect(s.bucketsObserved).toBe(0);
    expect(s.currentBucketFill).toBe(50);
  });

  test('bucket close advances counter', () => {
    const v = new VpinCalculator(100, 5);
    v.recordFill('long', 100);
    expect(v.snapshot().bucketsObserved).toBe(1);
  });
});
