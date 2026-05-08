import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_PROGRAM_ID,
  commitBufferPda,
  flpExposurePda,
  insuranceFundPda,
  marketPda,
  orderBufferPda,
  positionPda,
  traderStatePda,
} from '../src/pdas.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

describe('PDA derivation', () => {
  test('FLASH_BOOK_PROGRAM_ID is a valid 32-byte pubkey', () => {
    expect(FLASH_BOOK_PROGRAM_ID.toBytes()).toHaveLength(32);
  });

  test('marketPda deterministic', () => {
    const a = marketPda(SOL, USDC);
    const b = marketPda(SOL, USDC);
    expect(a.address.toBase58()).toBe(b.address.toBase58());
    expect(a.bump).toBe(b.bump);
  });

  test('marketPda differs with different mints', () => {
    const a = marketPda(SOL, USDC);
    const b = marketPda(USDC, SOL); // swap
    expect(a.address.toBase58()).not.toBe(b.address.toBase58());
  });

  test('orderBufferPda derives from market', () => {
    const market = marketPda(SOL, USDC).address;
    const buf = orderBufferPda(market);
    expect(buf.address.toBytes()).toHaveLength(32);
  });

  test('commitBufferPda derives from market', () => {
    const market = marketPda(SOL, USDC).address;
    const buf = commitBufferPda(market);
    expect(buf.address.toBytes()).toHaveLength(32);
    expect(buf.address.toBase58()).not.toBe(market.toBase58());
  });

  test('insuranceFundPda is global', () => {
    const a = insuranceFundPda();
    const b = insuranceFundPda();
    expect(a.address.toBase58()).toBe(b.address.toBase58());
  });

  test('flpExposurePda is global', () => {
    const a = flpExposurePda();
    const b = flpExposurePda();
    expect(a.address.toBase58()).toBe(b.address.toBase58());
  });

  test('traderStatePda differs per trader', () => {
    const t1 = Keypair.generate().publicKey;
    const t2 = Keypair.generate().publicKey;
    const a = traderStatePda(t1);
    const b = traderStatePda(t2);
    expect(a.address.toBase58()).not.toBe(b.address.toBase58());
  });

  test('positionPda differs per (market, trader) pair', () => {
    const m1 = marketPda(SOL, USDC).address;
    const m2 = marketPda(USDC, SOL).address;
    const trader = Keypair.generate().publicKey;
    const a = positionPda(m1, trader);
    const b = positionPda(m2, trader);
    expect(a.address.toBase58()).not.toBe(b.address.toBase58());
  });

  test('positionPda differs per trader on same market', () => {
    const market = marketPda(SOL, USDC).address;
    const t1 = Keypair.generate().publicKey;
    const t2 = Keypair.generate().publicKey;
    const a = positionPda(market, t1);
    const b = positionPda(market, t2);
    expect(a.address.toBase58()).not.toBe(b.address.toBase58());
  });

  test('all derived addresses have valid bumps', () => {
    const market = marketPda(SOL, USDC);
    const buf = orderBufferPda(market.address);
    const cbuf = commitBufferPda(market.address);
    const trader = Keypair.generate().publicKey;
    const ts = traderStatePda(trader);
    const pos = positionPda(market.address, trader);

    for (const d of [market, buf, cbuf, ts, pos]) {
      expect(d.bump).toBeGreaterThanOrEqual(0);
      expect(d.bump).toBeLessThan(256);
    }
  });
});
