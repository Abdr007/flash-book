import { describe, expect, test } from 'bun:test';
import { buildCommitHash, CommitRevealRegistry, type RevealPayload } from '../src/commit-reveal.ts';

function payload(overrides: Partial<RevealPayload> = {}): RevealPayload {
  return {
    market: 'SOL',
    trader: 'T',
    side: 'long',
    size: 1,
    limitPrice: 100,
    nonce: 'n1',
    ...overrides,
  };
}

describe('commit-reveal', () => {
  test('valid reveal redeems to taker order', () => {
    const reg = new CommitRevealRegistry();
    const p = payload();
    const hash = buildCommitHash(p);
    reg.registerCommit({
      hash,
      trader: p.trader,
      market: p.market,
      bondLamports: 100,
      currentBatch: 1,
      expireInBatches: 5,
    });
    const order = reg.redeem({ payload: p, currentBatch: 2, nowMs: 100, orderIdSeq: 1 });
    expect(order).not.toBeNull();
    expect(order!.type).toBe('taker');
    expect(order!.size).toBe(p.size);
    expect(order!.limitPrice).toBe(p.limitPrice);
    expect(reg.pendingCount()).toBe(0);
  });

  test('reveal with mismatched payload fails', () => {
    const reg = new CommitRevealRegistry();
    const p = payload();
    reg.registerCommit({
      hash: buildCommitHash(p),
      trader: p.trader,
      market: p.market,
      bondLamports: 100,
      currentBatch: 1,
      expireInBatches: 5,
    });
    // tamper with size
    const tampered = { ...p, size: 2 };
    const order = reg.redeem({ payload: tampered, currentBatch: 2, nowMs: 100, orderIdSeq: 1 });
    expect(order).toBeNull();
  });

  test('expired commit cannot be redeemed and bond is seized', () => {
    const reg = new CommitRevealRegistry();
    const p = payload();
    reg.registerCommit({
      hash: buildCommitHash(p),
      trader: p.trader,
      market: p.market,
      bondLamports: 100,
      currentBatch: 1,
      expireInBatches: 2,
    });
    const order = reg.redeem({ payload: p, currentBatch: 10, nowMs: 100, orderIdSeq: 1 });
    expect(order).toBeNull();
    const seized = reg.sweepExpired(10);
    expect(seized).toHaveLength(1);
    expect(seized[0]?.bond).toBe(100);
    expect(reg.totalSeizedBonds()).toBe(100);
    expect(reg.pendingCount()).toBe(0);
  });

  test('duplicate commit registration throws', () => {
    const reg = new CommitRevealRegistry();
    const p = payload();
    const hash = buildCommitHash(p);
    reg.registerCommit({ hash, trader: p.trader, market: p.market, bondLamports: 100, currentBatch: 1, expireInBatches: 5 });
    expect(() => reg.registerCommit({ hash, trader: p.trader, market: p.market, bondLamports: 100, currentBatch: 1, expireInBatches: 5 })).toThrow();
  });
});
