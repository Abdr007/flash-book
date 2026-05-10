import { describe, expect, test } from 'bun:test';
import {
  FLASH_BOOK_PROGRAM_ID,
  FLASH_BOOK_ORDERS_PROGRAM_ID,
  FLASH_BOOK_FLP_PROGRAM_ID,
  FLASH_BOOK_VAULTS_PROGRAM_ID,
} from '../src/index.ts';

describe('wave 21: modular program IDs', () => {
  test('all four IDs are valid 32-byte pubkeys', () => {
    for (const id of [
      FLASH_BOOK_PROGRAM_ID,
      FLASH_BOOK_ORDERS_PROGRAM_ID,
      FLASH_BOOK_FLP_PROGRAM_ID,
      FLASH_BOOK_VAULTS_PROGRAM_ID,
    ]) {
      expect(id.toBytes()).toHaveLength(32);
    }
  });

  test('all four IDs are distinct', () => {
    const ids = [
      FLASH_BOOK_PROGRAM_ID.toBase58(),
      FLASH_BOOK_ORDERS_PROGRAM_ID.toBase58(),
      FLASH_BOOK_FLP_PROGRAM_ID.toBase58(),
      FLASH_BOOK_VAULTS_PROGRAM_ID.toBase58(),
    ];
    expect(new Set(ids).size).toBe(4);
  });

  test('orders program ID matches Anchor.toml + on-chain declare_id!', () => {
    expect(FLASH_BOOK_ORDERS_PROGRAM_ID.toBase58()).toBe(
      '2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5',
    );
  });

  test('flp program ID matches Anchor.toml + on-chain declare_id!', () => {
    expect(FLASH_BOOK_FLP_PROGRAM_ID.toBase58()).toBe(
      'eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M',
    );
  });

  test('vaults program ID matches Anchor.toml + on-chain declare_id!', () => {
    expect(FLASH_BOOK_VAULTS_PROGRAM_ID.toBase58()).toBe(
      'GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt',
    );
  });
});
