import { describe, expect, test } from 'bun:test';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_FLP_PROGRAM_ID,
  FLASH_BOOK_ORDERS_PROGRAM_ID,
  FLASH_BOOK_VAULTS_PROGRAM_ID,
  marketPda,
  twapOrderPda,
  twapOrderV3Pda,
  icebergOrderPda,
  icebergOrderV3Pda,
  flpExposurePda,
  flpExposurePerMarketV3Pda,
  vaultPda,
  vaultV3Pda,
  vaultPositionPda,
  vaultPositionV3Pda,
} from '../src/index.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

describe('wave 21 phase 3b: TwapOrderAccountV3 PDA', () => {
  test('derives under flash-book-orders, distinct from core', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const v3 = twapOrderV3Pda(market, trader, 0);
    const v2 = twapOrderPda(market, trader, 0);
    expect(v3.address.toBytes()).toHaveLength(32);
    expect(v3.address.equals(v2.address)).toBe(false);
  });
});

describe('wave 21 phase 3c: IcebergOrderAccountV3 PDA', () => {
  test('derives under flash-book-orders, distinct from core', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const v3 = icebergOrderV3Pda(market, trader, 0);
    const v2 = icebergOrderPda(market, trader, 0);
    expect(v3.address.equals(v2.address)).toBe(false);
  });

  test('different iceberg_id → different address', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    expect(
      icebergOrderV3Pda(market, trader, 1).address.equals(
        icebergOrderV3Pda(market, trader, 2).address,
      ),
    ).toBe(false);
  });
});

describe('wave 21 phase 8: FlpExposurePerMarketAccountV3 PDA', () => {
  test('per-market under flash-book-flp; distinct from singleton', () => {
    const m1 = marketPda(SOL, USDC).address;
    const m2 = marketPda(USDC, SOL).address;
    const v3a = flpExposurePerMarketV3Pda(m1);
    const v3b = flpExposurePerMarketV3Pda(m2);
    const singleton = flpExposurePda();
    expect(v3a.address.toBytes()).toHaveLength(32);
    expect(v3a.address.equals(v3b.address)).toBe(false);
    expect(v3a.address.equals(singleton.address)).toBe(false);
  });

  test('determinism', () => {
    const m = marketPda(SOL, USDC).address;
    expect(
      flpExposurePerMarketV3Pda(m).address.equals(
        flpExposurePerMarketV3Pda(m).address,
      ),
    ).toBe(true);
  });

  test('correct derivation under flash-book-flp program ID', () => {
    const m = marketPda(SOL, USDC).address;
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('flp_per_market'), m.toBuffer()],
      FLASH_BOOK_FLP_PROGRAM_ID,
    );
    const got = flpExposurePerMarketV3Pda(m);
    expect(got.address.equals(expected[0])).toBe(true);
  });
});

describe('wave 21 phase 9: VaultAccountV3 / VaultPositionAccountV3 PDAs', () => {
  test('vault PDA under flash-book-vaults, distinct from core', () => {
    const strat = Keypair.generate().publicKey;
    const v3 = vaultV3Pda(strat, 0);
    const v2 = vaultPda(strat, 0);
    expect(v3.address.toBytes()).toHaveLength(32);
    expect(v3.address.equals(v2.address)).toBe(false);
  });

  test('vault position PDA under flash-book-vaults', () => {
    const strat = Keypair.generate().publicKey;
    const dep = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strat, 0).address;
    const v3 = vaultPositionV3Pda(vault, dep);
    const v2 = vaultPositionPda(vault, dep);
    expect(v3.address.toBytes()).toHaveLength(32);
    expect(v3.address.equals(v2.address)).toBe(false);
  });

  test('different vault_id → different address', () => {
    const strat = Keypair.generate().publicKey;
    expect(
      vaultV3Pda(strat, 0).address.equals(vaultV3Pda(strat, 1).address),
    ).toBe(false);
  });

  test('correct derivation under flash-book-vaults program ID', () => {
    const strat = Keypair.generate().publicKey;
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('vault_v3'), strat.toBuffer(), Buffer.from([0])],
      FLASH_BOOK_VAULTS_PROGRAM_ID,
    );
    expect(vaultV3Pda(strat, 0).address.equals(expected[0])).toBe(true);
  });
});

describe('wave 21: every wrapper-program PDA helper is namespaced cleanly', () => {
  test('all v3 PDAs differ from their v2 (core-owned) counterparts', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const strat = Keypair.generate().publicKey;
    expect(twapOrderV3Pda(market, trader, 0).address.equals(twapOrderPda(market, trader, 0).address)).toBe(false);
    expect(icebergOrderV3Pda(market, trader, 0).address.equals(icebergOrderPda(market, trader, 0).address)).toBe(false);
    expect(vaultV3Pda(strat, 0).address.equals(vaultPda(strat, 0).address)).toBe(false);
  });

  test('v3 PDAs use the correct wrapper program', () => {
    const market = marketPda(SOL, USDC).address;
    const trader = Keypair.generate().publicKey;
    const strat = Keypair.generate().publicKey;
    // Derivation by program ID — re-derive against the EXPECTED wrapper
    // program and ensure SDK output matches (catches accidental seed/
    // program-ID swaps).
    expect(
      twapOrderV3Pda(market, trader, 0).address.equals(
        PublicKey.findProgramAddressSync(
          [Buffer.from('twap_v3'), market.toBuffer(), trader.toBuffer(), Buffer.from([0])],
          FLASH_BOOK_ORDERS_PROGRAM_ID,
        )[0],
      ),
    ).toBe(true);
    expect(
      vaultV3Pda(strat, 0).address.equals(
        PublicKey.findProgramAddressSync(
          [Buffer.from('vault_v3'), strat.toBuffer(), Buffer.from([0])],
          FLASH_BOOK_VAULTS_PROGRAM_ID,
        )[0],
      ),
    ).toBe(true);
  });
});
