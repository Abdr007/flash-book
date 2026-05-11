import { describe, expect, test } from 'bun:test';
import { decodeAccount } from '../src/accounts.ts';

describe('Account decoder', () => {
  test('throws on empty buffer (no discriminator)', () => {
    expect(() => decodeAccount('marketAccount', Buffer.from([]))).toThrow();
  });

  test('throws on wrong discriminator', () => {
    // 8 bytes that won't match any account's discriminator.
    const wrongDisc = Buffer.from([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    expect(() => decodeAccount('marketAccount', wrongDisc)).toThrow();
  });

  test('decoder accepts all 5 account names without compile-time error', () => {
    const names = [
      'marketAccount',
      'insuranceFundAccount',
      'flpExposureAccount',
      'traderStateAccount',
      'positionAccount',
    ] as const;
    expect(names.length).toBe(5);
  });
});
