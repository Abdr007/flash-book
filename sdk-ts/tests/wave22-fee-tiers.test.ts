import { describe, expect, test } from 'bun:test';
import BN from 'bn.js';
import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  feeTiersPda,
} from '../src/index.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

describe('wave 22 phase 1: feeTiersPda', () => {
  test('singleton derivation under flash-book-core', () => {
    const a = feeTiersPda();
    const b = feeTiersPda();
    expect(a.address.toBytes()).toHaveLength(32);
    expect(a.address.equals(b.address)).toBe(true); // determinism
  });

  test('correct derivation against canonical seeds', () => {
    const expected = PublicKey.findProgramAddressSync(
      [Buffer.from('fee_tiers')],
      FLASH_BOOK_PROGRAM_ID,
    );
    expect(feeTiersPda().address.equals(expected[0])).toBe(true);
  });

  test('different program ID → different address', () => {
    const fakeProgram = Keypair.generate().publicKey;
    expect(
      feeTiersPda().address.equals(feeTiersPda(fakeProgram).address),
    ).toBe(false);
  });
});

describe('wave 22 phase 1: SDK builders', () => {
  // Real connection isn't required — Anchor builds the ix synchronously
  // from the IDL + accountsPartial. We're verifying the wire shape.
  const conn = new Connection('http://127.0.0.1:8899', 'processed');
  const wallet = {
    publicKey: Keypair.generate().publicKey,
    signTransaction: async (tx: any) => tx,
    signAllTransactions: async (txs: any[]) => txs,
  };
  const client = new FlashBookClient(conn, wallet as any);

  test('initFeeTiersIx — encodes 4-tier HL-style schedule', async () => {
    const authority = Keypair.generate().publicKey;
    const ix = await client.initFeeTiersIx({
      authority,
      volumeWindowSlots: new BN(3_024_000), // 14 days @ 0.4s
      tiers: [
        { minVolumeQuoteLots: new BN(0), makerRebateBps: 2, takerFeeBps: 5 },
        { minVolumeQuoteLots: new BN(1_000_000_000_000n.toString()), makerRebateBps: 3, takerFeeBps: 4 },
        { minVolumeQuoteLots: new BN(5_000_000_000_000n.toString()), makerRebateBps: 4, takerFeeBps: 3 },
        { minVolumeQuoteLots: new BN(25_000_000_000_000n.toString()), makerRebateBps: 6, takerFeeBps: 2 },
      ],
    });
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    expect(ix.keys.length).toBeGreaterThan(0);
    // FeeTiers PDA must appear in the account list (init account).
    const ftPda = feeTiersPda().address;
    expect(ix.keys.some((k) => k.pubkey.equals(ftPda))).toBe(true);
  });

  test('updateFeeTiersIx — encodes single-tier reset to default rates', async () => {
    const authority = Keypair.generate().publicKey;
    const ix = await client.updateFeeTiersIx({
      authority,
      volumeWindowSlots: new BN(3_024_000),
      tiers: [{ minVolumeQuoteLots: new BN(0), makerRebateBps: 0, takerFeeBps: 5 }],
    });
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    const ftPda = feeTiersPda().address;
    expect(ix.keys.some((k) => k.pubkey.equals(ftPda))).toBe(true);
  });

  test('viewTraderEffectiveTierIx — wires trader + traderState + feeTiers', async () => {
    const trader = Keypair.generate().publicKey;
    const ix = await client.viewTraderEffectiveTierIx({ trader });
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    const ftPda = feeTiersPda().address;
    expect(ix.keys.some((k) => k.pubkey.equals(ftPda))).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(trader))).toBe(true);
  });
});

// Unused import suppression — SOL/USDC are placeholders for future
// integration-level tests that exercise the full apply_fill volume
// crediting path against a localnet validator.
void SOL;
void USDC;
