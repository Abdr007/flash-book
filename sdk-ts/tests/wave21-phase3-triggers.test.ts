import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_ORDERS_PROGRAM_ID,
  FLASH_BOOK_FLP_PROGRAM_ID,
  FLASH_BOOK_VAULTS_PROGRAM_ID,
  marketPda,
  triggerOrderPda,
  triggerOrderV3Pda,
  wrapperCpiAuthorityPda,
} from '../src/index.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

describe('wave 21 phase 3a: TriggerOrderAccountV3 PDA', () => {
  test('derives a valid 32-byte pubkey under the orders program', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const tier = triggerOrderV3Pda(market, trader, 0);
    expect(tier.address.toBytes()).toHaveLength(32);
    expect(tier.bump).toBeGreaterThanOrEqual(0);
    expect(tier.bump).toBeLessThan(256);
  });

  test('seed prefix differs from core triggerOrderPda → distinct addresses', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const v3 = triggerOrderV3Pda(market, trader, 0);
    const v2 = triggerOrderPda(market, trader, 0);
    // v3 lives under orders program, v2 lives under core; addresses
    // MUST differ so the trader can hold both during migration.
    expect(v3.address.equals(v2.address)).toBe(false);
  });

  test('different trigger_ids → different addresses', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    expect(
      triggerOrderV3Pda(market, trader, 0).address.equals(
        triggerOrderV3Pda(market, trader, 1).address,
      ),
    ).toBe(false);
  });

  test('different traders → different addresses', () => {
    const market = marketPda(SOL, USDC).address;
    const t1 = Keypair.generate().publicKey;
    const t2 = Keypair.generate().publicKey;
    expect(
      triggerOrderV3Pda(market, t1, 0).address.equals(
        triggerOrderV3Pda(market, t2, 0).address,
      ),
    ).toBe(false);
  });

  test('determinism — same inputs produce same PDA', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    expect(
      triggerOrderV3Pda(market, trader, 5).address.equals(
        triggerOrderV3Pda(market, trader, 5).address,
      ),
    ).toBe(true);
  });
});

describe('wave 21 phase 2: wrapper CPI authority PDAs', () => {
  test('all 3 wrapper programs derive distinct CPI authorities', () => {
    const orders = wrapperCpiAuthorityPda(FLASH_BOOK_ORDERS_PROGRAM_ID);
    const flp = wrapperCpiAuthorityPda(FLASH_BOOK_FLP_PROGRAM_ID);
    const vaults = wrapperCpiAuthorityPda(FLASH_BOOK_VAULTS_PROGRAM_ID);
    const set = new Set([
      orders.address.toBase58(),
      flp.address.toBase58(),
      vaults.address.toBase58(),
    ]);
    expect(set.size).toBe(3);
    for (const pda of [orders, flp, vaults]) {
      expect(pda.address.toBytes()).toHaveLength(32);
    }
  });

  test('CPI authority derivation is deterministic', () => {
    const a = wrapperCpiAuthorityPda(FLASH_BOOK_ORDERS_PROGRAM_ID);
    const b = wrapperCpiAuthorityPda(FLASH_BOOK_ORDERS_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
    expect(a.bump).toBe(b.bump);
  });

  test('CPI authority address matches what core checks', () => {
    // The on-chain check (lib.rs:place_limit_order_v2_cpi) computes:
    //   find_program_address(&[CPI_AUTHORITY_SEED], &orders_program_id)
    // SDK helper must produce the SAME bytes.
    const orders = wrapperCpiAuthorityPda(FLASH_BOOK_ORDERS_PROGRAM_ID);
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('cpi_authority')],
      FLASH_BOOK_ORDERS_PROGRAM_ID,
    );
    expect(orders.address.equals(expected[0])).toBe(true);
    expect(orders.bump).toBe(expected[1]);
  });
});
