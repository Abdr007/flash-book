// Tests for projectPosition — the post-fill state projection used inside
// previewTrade. Mirrors the on-chain `apply_fill_to_position` semantics.

import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import { projectPosition } from '../src/preview-trade.ts';
import type { PositionAccount } from '../src/accounts.ts';

const trader = Keypair.generate().publicKey;
const market = Keypair.generate().publicKey;

function pos(side: 0 | 1, size: number, entry: number): PositionAccount {
  return {
    trader,
    market,
    bump: 0,
    side,
    sizeLots: new BN(size),
    entryPriceTicks: new BN(entry),
    collateralQuoteLots: new BN(0),
    cumFundingIndexAtEntry: new BN(0),
    realizedPnlQuoteLots: new BN(0),
    fundingPaidQuoteLots: new BN(0),
    lastSettlementBatch: new BN(0),
  };
}

describe('projectPosition', () => {
  test('null current → opens new position with fill values', () => {
    const out = projectPosition(null, 'long', 10, 100, trader, market);
    expect(out.side).toBe(0);
    expect(out.sizeLots.toNumber()).toBe(10);
    expect(out.entryPriceTicks.toNumber()).toBe(100);
  });

  test('zero-size current → opens new position', () => {
    const out = projectPosition(pos(0, 0, 0), 'short', 5, 200, trader, market);
    expect(out.side).toBe(1);
    expect(out.sizeLots.toNumber()).toBe(5);
    expect(out.entryPriceTicks.toNumber()).toBe(200);
  });

  test('same side → volume-weighted average entry', () => {
    // Existing 10 @ 100, add 10 @ 200 → 20 @ 150.
    const out = projectPosition(pos(0, 10, 100), 'long', 10, 200, trader, market);
    expect(out.side).toBe(0);
    expect(out.sizeLots.toNumber()).toBe(20);
    expect(out.entryPriceTicks.toNumber()).toBe(150);
  });

  test('same side asymmetric weights', () => {
    // Existing 5 @ 100, add 15 @ 200 → 20 @ 175.
    const out = projectPosition(pos(0, 5, 100), 'long', 15, 200, trader, market);
    expect(out.sizeLots.toNumber()).toBe(20);
    expect(out.entryPriceTicks.toNumber()).toBe(175);
  });

  test('opposite side, smaller fill → reduce', () => {
    const out = projectPosition(pos(0, 10, 100), 'short', 3, 105, trader, market);
    expect(out.side).toBe(0); // still long
    expect(out.sizeLots.toNumber()).toBe(7);
    expect(out.entryPriceTicks.toNumber()).toBe(100); // entry unchanged
  });

  test('opposite side, exact size → close to zero', () => {
    const out = projectPosition(pos(0, 10, 100), 'short', 10, 105, trader, market);
    expect(out.sizeLots.toNumber()).toBe(0);
    expect(out.entryPriceTicks.toNumber()).toBe(0);
  });

  test('opposite side, larger fill → flip side', () => {
    // Long 10 @ 100, short 15 @ 105 → short 5 @ 105.
    const out = projectPosition(pos(0, 10, 100), 'short', 15, 105, trader, market);
    expect(out.side).toBe(1); // flipped to short
    expect(out.sizeLots.toNumber()).toBe(5);
    expect(out.entryPriceTicks.toNumber()).toBe(105);
  });

  test('flip preserves identity (trader, market)', () => {
    const out = projectPosition(pos(1, 5, 200), 'long', 8, 195, trader, market);
    expect(out.trader).toEqual(trader);
    expect(out.market).toEqual(market);
  });

  test('null current preserves identity', () => {
    const out = projectPosition(null, 'long', 1, 100, trader, market);
    expect(out.trader).toEqual(trader);
    expect(out.market).toEqual(market);
  });
});
