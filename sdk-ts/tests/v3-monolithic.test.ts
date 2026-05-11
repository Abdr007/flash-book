// Wave 23 monolithic merge: the 3 wrapper programs (orders / flp /
// vaults) were collapsed into flash_book. All V3 PDAs now derive under
// FLASH_BOOK_PROGRAM_ID. This file pins:
//   • The PDA derivations against known-good fixtures
//   • Each V3 PDA helper accepts the core program ID by default
//   • V3 seeds remain distinct from legacy v1 seeds

import { describe, expect, test } from 'bun:test';
import { PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_PROGRAM_ID,
  triggerOrderPda,
  triggerOrderV3Pda,
  twapOrderPda,
  twapOrderV3Pda,
  icebergOrderPda,
  icebergOrderV3Pda,
  flpExposurePerMarketV3Pda,
  flpPositionV3Pda,
  vaultV3Pda,
  vaultPositionV3Pda,
} from '../src/index.ts';

const MARKET = new PublicKey('So11111111111111111111111111111111111111112');
const TRADER = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');

describe('wave 23 monolithic: V3 PDAs derive under flash_book program ID', () => {
  test('trigger v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const a = triggerOrderV3Pda(MARKET, TRADER, 7);
    const b = triggerOrderV3Pda(MARKET, TRADER, 7, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('twap v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const a = twapOrderV3Pda(MARKET, TRADER, 3);
    const b = twapOrderV3Pda(MARKET, TRADER, 3, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('iceberg v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const a = icebergOrderV3Pda(MARKET, TRADER, 1);
    const b = icebergOrderV3Pda(MARKET, TRADER, 1, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('flp-per-market v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const a = flpExposurePerMarketV3Pda(MARKET);
    const b = flpExposurePerMarketV3Pda(MARKET, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('flp-position v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const exposure = flpExposurePerMarketV3Pda(MARKET).address;
    const a = flpPositionV3Pda(exposure, TRADER);
    const b = flpPositionV3Pda(exposure, TRADER, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('vault v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const a = vaultV3Pda(TRADER, 2);
    const b = vaultV3Pda(TRADER, 2, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });

  test('vault-position v3 PDA defaults to FLASH_BOOK_PROGRAM_ID', () => {
    const vault = vaultV3Pda(TRADER, 2).address;
    const a = vaultPositionV3Pda(vault, TRADER);
    const b = vaultPositionV3Pda(vault, TRADER, FLASH_BOOK_PROGRAM_ID);
    expect(a.address.equals(b.address)).toBe(true);
  });
});

describe('wave 23: legacy v1 seeds remain distinct from v3 seeds', () => {
  test('legacy `trigger` and v3 `trigger_v3` PDAs differ', () => {
    const v1 = triggerOrderPda(MARKET, TRADER, 0);
    const v3 = triggerOrderV3Pda(MARKET, TRADER, 0);
    expect(v1.address.equals(v3.address)).toBe(false);
  });

  test('legacy `twap` and v3 `twap_v3` PDAs differ', () => {
    const v1 = twapOrderPda(MARKET, TRADER, 0);
    const v3 = twapOrderV3Pda(MARKET, TRADER, 0);
    expect(v1.address.equals(v3.address)).toBe(false);
  });

  test('legacy `iceberg` and v3 `iceberg_v3` PDAs differ', () => {
    const v1 = icebergOrderPda(MARKET, TRADER, 0);
    const v3 = icebergOrderV3Pda(MARKET, TRADER, 0);
    expect(v1.address.equals(v3.address)).toBe(false);
  });
});
