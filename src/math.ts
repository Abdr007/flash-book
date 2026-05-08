// Numeric helpers. All financial math goes through these.

export function clamp(x: number, lo: number, hi: number): number {
  if (x < lo) return lo;
  if (x > hi) return hi;
  return x;
}

export function safeNumber(x: number, fallback = 0): number {
  return Number.isFinite(x) ? x : fallback;
}

export function roundToTick(price: number, tickSize: number): number {
  if (tickSize <= 0) return price;
  return Math.round(price / tickSize) * tickSize;
}

export function roundLot(size: number, minLot: number): number {
  if (minLot <= 0) return size;
  return Math.floor(size / minLot) * minLot;
}

// Deterministic RNG (xorshift32) so simulations are reproducible.
export class Prng {
  private state: number;

  constructor(seed: number) {
    this.state = seed === 0 ? 0x12345678 : seed >>> 0;
  }

  next(): number {
    let x = this.state;
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    this.state = x >>> 0;
    return this.state / 0x100000000;
  }

  range(lo: number, hi: number): number {
    return lo + this.next() * (hi - lo);
  }

  int(lo: number, hi: number): number {
    return Math.floor(this.range(lo, hi + 1));
  }

  bool(p = 0.5): boolean {
    return this.next() < p;
  }

  pick<T>(arr: ReadonlyArray<T>): T {
    if (arr.length === 0) throw new Error('Prng.pick: empty array');
    const i = this.int(0, arr.length - 1);
    return arr[i] as T;
  }

  // Box-Muller standard normal
  normal(mean = 0, stdDev = 1): number {
    let u1 = this.next();
    if (u1 === 0) u1 = 1e-10;
    const u2 = this.next();
    const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
    return mean + z * stdDev;
  }
}

// EMA helper.
export function emaUpdate(prev: number, newSample: number, window: number): number {
  if (window <= 1) return newSample;
  const alpha = 2 / (window + 1);
  return prev * (1 - alpha) + newSample * alpha;
}

// Trim TWAP buffer and compute mean.
export function pushAndTwap(buf: number[], value: number, maxLen: number): number {
  buf.push(value);
  while (buf.length > maxLen) buf.shift();
  if (buf.length === 0) return value;
  let sum = 0;
  for (const v of buf) sum += v;
  return sum / buf.length;
}

// Banded value: clamp `inner` within [outer*(1-band), outer*(1+band)].
export function oracleBand(inner: number, outer: number, bandBps: number): number {
  const band = Math.abs(outer) * (bandBps / 10_000);
  return clamp(inner, outer - band, outer + band);
}

// Stable hash for commit-reveal (FNV-1a variant). Production would use a real hash;
// this is fine for a deterministic simulator and avoids a dependency.
export function commitHash(parts: ReadonlyArray<string | number>): string {
  let h = 0xcbf29ce484222325n; // FNV offset basis
  const prime = 0x100000001b3n;
  const mod = 1n << 64n;
  const mask = mod - 1n;
  for (const p of parts) {
    const s = String(p);
    for (let i = 0; i < s.length; i++) {
      h = (h ^ BigInt(s.charCodeAt(i))) & mask;
      h = (h * prime) & mask;
    }
    h = (h ^ 0xffn) & mask;
    h = (h * prime) & mask;
  }
  return h.toString(16).padStart(16, '0');
}

// Total of an array — guard NaN propagation.
export function sumSafe(xs: ReadonlyArray<number>): number {
  let total = 0;
  for (const x of xs) total += safeNumber(x);
  return total;
}
