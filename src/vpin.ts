// VPIN — Volume-synchronized Probability of Informed Trading.
// Easley, López de Prado, O'Hara (2012).
//
// Each fixed-volume bucket records imbalance |V_buy - V_sell| / V.
// VPIN is the EMA of this imbalance over the last `emaWindow` buckets.
//
// In an FBA matcher we know each fill's taker side directly,
// so we don't need the Lee-Ready tick test — we use the side as ground truth.

import { emaUpdate } from './math.ts';
import type { Side } from './types.ts';

export interface VpinSnapshot {
  readonly value: number;
  readonly bucketsObserved: number;
  readonly currentBucketFill: number;
}

export class VpinCalculator {
  private buyVol = 0;
  private sellVol = 0;
  private bucketsObserved = 0;
  private vpin = 0;

  constructor(
    private readonly bucketSize: number,
    private readonly emaWindow: number,
  ) {
    if (bucketSize <= 0) throw new Error('VPIN bucketSize must be > 0');
    if (emaWindow <= 0) throw new Error('VPIN emaWindow must be > 0');
  }

  recordFill(takerSide: Side, size: number): void {
    if (!Number.isFinite(size) || size <= 0) return;
    if (takerSide === 'long') this.buyVol += size;
    else this.sellVol += size;

    const total = this.buyVol + this.sellVol;
    while (total >= this.bucketSize) {
      // Close out one bucket-worth of volume.
      const ratio = this.bucketSize / total;
      const buyChunk = this.buyVol * ratio;
      const sellChunk = this.sellVol * ratio;
      const imbalance = Math.abs(buyChunk - sellChunk) / this.bucketSize;
      this.vpin = emaUpdate(this.vpin, imbalance, this.emaWindow);
      this.bucketsObserved += 1;
      this.buyVol -= buyChunk;
      this.sellVol -= sellChunk;
      // If we still have ≥ bucketSize left, loop closes the next bucket.
      if (this.buyVol + this.sellVol < this.bucketSize) break;
    }
  }

  get value(): number {
    return this.vpin;
  }

  snapshot(): VpinSnapshot {
    return {
      value: this.vpin,
      bucketsObserved: this.bucketsObserved,
      currentBucketFill: this.buyVol + this.sellVol,
    };
  }
}
