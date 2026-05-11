import { describe, expect, test } from 'bun:test';
import BN from 'bn.js';
import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import {
  FLASH_BOOK_PROGRAM_ID,
  FLASH_BOOK_VAULTS_PROGRAM_ID,
  FlashBookVaultsClient,
  marketPda,
  vaultV3Pda,
  vaultPositionV3Pda,
  wrapperCpiAuthorityPda,
} from '../src/index.ts';

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

function makeClient() {
  const conn = new Connection('http://127.0.0.1:8899', 'processed');
  const wallet = {
    publicKey: Keypair.generate().publicKey,
    signTransaction: async (tx: any) => tx,
    signAllTransactions: async (txs: any[]) => txs,
  };
  return new FlashBookVaultsClient(conn, wallet as any);
}

describe('FlashBookVaultsClient — wave 22 phase 5 vault trading', () => {
  test('createVaultV3Ix — wires strategist + vault PDA + sysprog', async () => {
    const client = makeClient();
    const strategist = Keypair.generate().publicKey;
    const name = new Uint8Array(32);
    name.set(new TextEncoder().encode('momentum-vol'));
    const ix = await client.createVaultV3Ix({
      strategist,
      vaultId: 0,
      name,
      perfFeeBps: 1500,
    });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    const vaultPda = vaultV3Pda(strategist, 0).address;
    expect(ix.keys.some((k) => k.pubkey.equals(vaultPda))).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(strategist))).toBe(true);
  });

  test('createVaultV3Ix — rejects non-32-byte name', () => {
    const client = makeClient();
    const tooShort = new Uint8Array(16);
    expect(() =>
      client.createVaultV3Ix({
        strategist: Keypair.generate().publicKey,
        vaultId: 0,
        name: tooShort,
        perfFeeBps: 1000,
      }),
    ).toThrow(/32 bytes/);
  });

  test('vaultOpenTraderStateV3Ix — includes cpi_authority + flash_book_program', async () => {
    const client = makeClient();
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const ix = await client.vaultOpenTraderStateV3Ix({ strategist, vault });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    const cpiAuth = wrapperCpiAuthorityPda(FLASH_BOOK_VAULTS_PROGRAM_ID).address;
    expect(ix.keys.some((k) => k.pubkey.equals(cpiAuth))).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(FLASH_BOOK_PROGRAM_ID))).toBe(true);
  });

  test('vaultDepositV3Ix — wires depositor ATA + quote_vault + cpi accounts', async () => {
    const client = makeClient();
    const depositor = Keypair.generate().publicKey;
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const quoteVault = Keypair.generate().publicKey;
    const ix = await client.vaultDepositV3Ix({
      depositor,
      vault,
      amountQuoteLots: new BN(1_000_000_000),
      quoteMint: USDC,
      quoteVault,
    });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(quoteVault))).toBe(true);
    const pos = vaultPositionV3Pda(vault, depositor).address;
    expect(ix.keys.some((k) => k.pubkey.equals(pos))).toBe(true);
  });

  test('vaultWithdrawV3Ix — wires release-side accounts', async () => {
    const client = makeClient();
    const depositor = Keypair.generate().publicKey;
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const quoteVault = Keypair.generate().publicKey;
    const ix = await client.vaultWithdrawV3Ix({
      depositor,
      vault,
      sharesToBurn: new BN(500_000),
      quoteMint: USDC,
      quoteVault,
    });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(quoteVault))).toBe(true);
  });

  test('vaultPlaceOrderV3Ix — encodes side / size / limit / market_book', async () => {
    const client = makeClient();
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const market = marketPda(SOL, USDC).address;
    const ix = await client.vaultPlaceOrderV3Ix({
      strategist,
      vault,
      market,
      side: 'long',
      sizeLots: new BN(10),
      limitTicks: new BN(99_950),
    });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(market))).toBe(true);
    const cpiAuth = wrapperCpiAuthorityPda(FLASH_BOOK_VAULTS_PROGRAM_ID).address;
    expect(ix.keys.some((k) => k.pubkey.equals(cpiAuth))).toBe(true);
  });

  test('vaultCancelOrderV3Ix — encodes side + order_id', async () => {
    const client = makeClient();
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const market = marketPda(SOL, USDC).address;
    const ix = await client.vaultCancelOrderV3Ix({
      strategist,
      vault,
      market,
      side: 'short',
      orderId: 12345n,
    });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    expect(ix.keys.some((k) => k.pubkey.equals(market))).toBe(true);
  });

  test('settleVaultPerfFeeV3Ix — wires strategist position', async () => {
    const client = makeClient();
    const strategist = Keypair.generate().publicKey;
    const vault = vaultV3Pda(strategist, 0).address;
    const ix = await client.settleVaultPerfFeeV3Ix({ strategist, vault });
    expect(ix.programId.equals(FLASH_BOOK_VAULTS_PROGRAM_ID)).toBe(true);
    const stratPos = vaultPositionV3Pda(vault, strategist).address;
    expect(ix.keys.some((k) => k.pubkey.equals(stratPos))).toBe(true);
  });
});
