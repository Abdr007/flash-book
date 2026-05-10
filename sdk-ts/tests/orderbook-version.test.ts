import { describe, expect, test } from 'bun:test';
import { PublicKey } from '@solana/web3.js';
import { detectOrderbookVersion, PREFERRED_ORDERBOOK_VERSION } from '../src/index.ts';
import { marketBookPda, orderBufferPda } from '../src/pdas.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
const FAKE_MARKET = new PublicKey('11111111111111111111111111111112');

describe('wave 18h: orderbook version signal', () => {
  test('PREFERRED_ORDERBOOK_VERSION is v2', () => {
    expect(PREFERRED_ORDERBOOK_VERSION).toBe('v2');
  });

  test('detectOrderbookVersion returns "neither" for a market with no books', async () => {
    // Mock connection where every getAccountInfo returns null.
    const conn = {
      getAccountInfo: async (_pk: PublicKey) => null,
    } as unknown as import('@solana/web3.js').Connection;
    const result = await detectOrderbookVersion(conn, FAKE_MARKET);
    expect(result).toBe('neither');
  });

  test('detectOrderbookVersion returns "v1" when only order_buffer is live', async () => {
    const v1Pda = orderBufferPda(FAKE_MARKET).address;
    const v2Pda = marketBookPda(FAKE_MARKET).address;
    const conn = {
      getAccountInfo: async (pk: PublicKey) => {
        if (pk.equals(v1Pda)) {
          return { data: Buffer.alloc(100), executable: false, lamports: 1, owner: SOL, rentEpoch: 0 } as unknown as ReturnType<import('@solana/web3.js').Connection['getAccountInfo']>;
        }
        if (pk.equals(v2Pda)) return null;
        return null;
      },
    } as unknown as import('@solana/web3.js').Connection;
    expect(await detectOrderbookVersion(conn, FAKE_MARKET)).toBe('v1');
  });

  test('detectOrderbookVersion returns "v2" when only market_book is live', async () => {
    const v1Pda = orderBufferPda(FAKE_MARKET).address;
    const v2Pda = marketBookPda(FAKE_MARKET).address;
    const conn = {
      getAccountInfo: async (pk: PublicKey) => {
        if (pk.equals(v1Pda)) return null;
        if (pk.equals(v2Pda)) {
          return { data: Buffer.alloc(9864), executable: false, lamports: 1, owner: USDC, rentEpoch: 0 } as unknown as ReturnType<import('@solana/web3.js').Connection['getAccountInfo']>;
        }
        return null;
      },
    } as unknown as import('@solana/web3.js').Connection;
    expect(await detectOrderbookVersion(conn, FAKE_MARKET)).toBe('v2');
  });

  test('detectOrderbookVersion returns "both" when both books are live', async () => {
    const conn = {
      getAccountInfo: async (_pk: PublicKey) =>
        ({ data: Buffer.alloc(100), executable: false, lamports: 1, owner: SOL, rentEpoch: 0 }) as unknown as ReturnType<import('@solana/web3.js').Connection['getAccountInfo']>,
    } as unknown as import('@solana/web3.js').Connection;
    expect(await detectOrderbookVersion(conn, FAKE_MARKET)).toBe('both');
  });

  test('detectOrderbookVersion treats zero-length data as not-live', async () => {
    const conn = {
      getAccountInfo: async (_pk: PublicKey) =>
        ({ data: Buffer.alloc(0), executable: false, lamports: 1, owner: SOL, rentEpoch: 0 }) as unknown as ReturnType<import('@solana/web3.js').Connection['getAccountInfo']>,
    } as unknown as import('@solana/web3.js').Connection;
    expect(await detectOrderbookVersion(conn, FAKE_MARKET)).toBe('neither');
  });
});
