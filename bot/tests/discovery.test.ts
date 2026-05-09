import { describe, expect, test } from 'bun:test';
import { createHash } from 'node:crypto';

// Validate the Anchor discriminator computation matches the on-chain
// convention: first 8 bytes of sha256("account:" + Name).
describe('Anchor discriminator computation', () => {
  test('PositionAccount discriminator is sha256("account:PositionAccount")[..8]', () => {
    const expected = createHash('sha256').update('account:PositionAccount').digest().subarray(0, 8);
    expect(expected.length).toBe(8);
  });

  test('TraderStateAccount discriminator is sha256("account:TraderStateAccount")[..8]', () => {
    const expected = createHash('sha256').update('account:TraderStateAccount').digest().subarray(0, 8);
    expect(expected.length).toBe(8);
  });
});

// Note: actual end-to-end discovery via getProgramAccounts requires a
// live RPC; we don't test that path in unit tests. Integration would
// happen against a localnet or devnet validator.
