import { describe, expect, test } from 'bun:test';
import { PublicKey } from '@solana/web3.js';
import {
  MAGICBLOCK_DELEGATION_PROGRAM_ID,
  delegateBufferPda,
  delegationRecordPda,
  delegationMetadataPda,
  marketBookPda,
  FLASH_BOOK_PROGRAM_ID,
} from '../src/index.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
const FAKE_MARKET = new PublicKey('11111111111111111111111111111112');

describe('wave 19b: MagicBlock ER delegation PDAs', () => {
  test('MAGICBLOCK_DELEGATION_PROGRAM_ID is the canonical address', () => {
    expect(MAGICBLOCK_DELEGATION_PROGRAM_ID.toBase58()).toBe(
      'DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh',
    );
    expect(MAGICBLOCK_DELEGATION_PROGRAM_ID.toBytes()).toHaveLength(32);
  });

  test('delegateBufferPda lives under owner program (= our program by default)', () => {
    const market = FAKE_MARKET;
    const buffer1 = delegateBufferPda(market);
    const buffer2 = delegateBufferPda(market, FLASH_BOOK_PROGRAM_ID);
    expect(buffer1.address.equals(buffer2.address)).toBe(true);
    expect(buffer1.bump).toBe(buffer2.bump);

    // Different owner program → different PDA.
    const buffer_other = delegateBufferPda(market, SOL);
    expect(buffer1.address.equals(buffer_other.address)).toBe(false);
  });

  test('delegationRecordPda lives under MAGICBLOCK_DELEGATION_PROGRAM_ID', () => {
    const market = FAKE_MARKET;
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('delegation'), market.toBuffer()],
      MAGICBLOCK_DELEGATION_PROGRAM_ID,
    );
    const derived = delegationRecordPda(market);
    expect(derived.address.equals(expected[0])).toBe(true);
    expect(derived.bump).toBe(expected[1]);
  });

  test('delegationMetadataPda uses delegation-metadata seed prefix', () => {
    const market = FAKE_MARKET;
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('delegation-metadata'), market.toBuffer()],
      MAGICBLOCK_DELEGATION_PROGRAM_ID,
    );
    const derived = delegationMetadataPda(market);
    expect(derived.address.equals(expected[0])).toBe(true);
  });

  test('all three delegation PDAs are distinct for the same delegated account', () => {
    const market = FAKE_MARKET;
    const buf = delegateBufferPda(market);
    const rec = delegationRecordPda(market);
    const meta = delegationMetadataPda(market);
    expect(buf.address.equals(rec.address)).toBe(false);
    expect(buf.address.equals(meta.address)).toBe(false);
    expect(rec.address.equals(meta.address)).toBe(false);
  });

  test('different delegated accounts produce different buffer/record/metadata PDAs', () => {
    const market_book_a = marketBookPda(USDC);
    const market_book_b = marketBookPda(SOL);
    expect(
      delegateBufferPda(market_book_a.address).address.equals(
        delegateBufferPda(market_book_b.address).address,
      ),
    ).toBe(false);
    expect(
      delegationRecordPda(market_book_a.address).address.equals(
        delegationRecordPda(market_book_b.address).address,
      ),
    ).toBe(false);
  });

  test('PDA derivation is deterministic across calls', () => {
    const market = FAKE_MARKET;
    expect(
      delegateBufferPda(market).address.equals(delegateBufferPda(market).address),
    ).toBe(true);
    expect(
      delegationRecordPda(market).address.equals(
        delegationRecordPda(market).address,
      ),
    ).toBe(true);
  });
});
