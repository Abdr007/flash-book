import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  FLASH_BOOK_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  flpExposurePda,
  insuranceFundPda,
  marketBookPda,
  marketPda,
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

  test('marketBookPda derives from market', () => {
    const market = marketPda(SOL, USDC).address;
    const buf = marketBookPda(market);
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

  test('TOKEN_PROGRAM_ID is the canonical SPL Token program', () => {
    expect(TOKEN_PROGRAM_ID.toBase58()).toBe(
      'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
    );
  });

  test('ASSOCIATED_TOKEN_PROGRAM_ID is the canonical ATA program', () => {
    expect(ASSOCIATED_TOKEN_PROGRAM_ID.toBase58()).toBe(
      'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL',
    );
  });

  test('associatedTokenAddress is deterministic', () => {
    const owner = Keypair.generate().publicKey;
    const a = associatedTokenAddress(owner, USDC);
    const b = associatedTokenAddress(owner, USDC);
    expect(a.toBase58()).toBe(b.toBase58());
  });

  test('associatedTokenAddress differs per (owner, mint) pair', () => {
    const o1 = Keypair.generate().publicKey;
    const o2 = Keypair.generate().publicKey;
    expect(associatedTokenAddress(o1, USDC).toBase58()).not.toBe(
      associatedTokenAddress(o2, USDC).toBase58(),
    );
    expect(associatedTokenAddress(o1, USDC).toBase58()).not.toBe(
      associatedTokenAddress(o1, SOL).toBase58(),
    );
  });

  test('associatedTokenAddress matches the standard SPL derivation', () => {
    // Reference value for ATA(owner = SOL native mint, mint = USDC).
    // The Rust integration tests cross-validate this derivation end-to-end:
    // they pass our-derived ATAs into the Anchor program, which uses
    // `associated_token::*` constraints to re-derive and reject any address
    // that doesn't match the canonical SPL formula. A green Rust suite
    // proves SDK ↔ on-chain agreement.
    const got = associatedTokenAddress(SOL, USDC).toBase58();
    expect(got).toBe('DHe62eeQVEnNK7vg5xUpDkJm7tuqHadjhvmPRFBG9UPo');
  });

  test('all derived addresses have valid bumps', () => {
    const market = marketPda(SOL, USDC);
    const book = marketBookPda(market.address);
    const trader = Keypair.generate().publicKey;
    const ts = traderStatePda(trader);
    const pos = positionPda(market.address, trader);

    for (const d of [market, book, ts, pos]) {
      expect(d.bump).toBeGreaterThanOrEqual(0);
      expect(d.bump).toBeLessThan(256);
    }
  });
});
