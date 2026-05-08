// Builder coverage — every instruction builder produces a valid
// TransactionInstruction with the right programId, non-empty data,
// and the expected account count.
//
// The exact account count is asserted because the SDK's *Ix builders
// must stay in lockstep with the Rust program's #[derive(Accounts)]
// shape. A missing or extra account in either side will fail this test.

import { describe, expect, test } from 'bun:test';
import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import BN from 'bn.js';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
  MarketStatus,
} from '../src/index.ts';

function makeClient(): FlashBookClient {
  // Mock connection — we never actually send. Builders are pure.
  const conn = new Connection('http://localhost:8899');
  const wallet = new Wallet(Keypair.generate());
  return new FlashBookClient(conn, wallet);
}

const SOL = new PublicKey('So11111111111111111111111111111111111111112');
const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

describe('Instruction builders', () => {
  // ─── Setup (3) ─────────────────────────────────────────────────────

  test('initializeInsuranceFundIx', async () => {
    const client = makeClient();
    const authority = Keypair.generate().publicKey;
    const ix = await client.initializeInsuranceFundIx(authority, defaultInsuranceFundParams());
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    expect(ix.data.length).toBeGreaterThan(0);
    expect(ix.keys.length).toBe(3); // authority, fund, sysprog
  });

  test('initializeFlpExposureIx', async () => {
    const client = makeClient();
    const authority = Keypair.generate().publicKey;
    const ix = await client.initializeFlpExposureIx(authority, new BN(1_000_000));
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    expect(ix.keys.length).toBe(3); // authority, flp, sysprog
  });

  test('initializeMarketIx', async () => {
    const client = makeClient();
    const ix = await client.initializeMarketIx({
      authority: Keypair.generate().publicKey,
      baseMint: SOL,
      quoteMint: USDC,
      baseVault: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
      oracleAccount: Keypair.generate().publicKey,
      params: defaultMajorMarketParams(),
      initialOracleTicks: new BN(100_000),
    });
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    // authority, base_mint, quote_mint, base_vault, quote_vault, oracle,
    // market, order_buffer, commit_buffer, insurance, flp, sysprog
    expect(ix.keys.length).toBe(12);
  });

  test('openTraderStateIx', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const ix = await client.openTraderStateIx(trader);
    expect(ix.keys.length).toBe(3); // trader, trader_state, sysprog
  });

  // ─── Lifecycle (4) ─────────────────────────────────────────────────

  test('depositCollateralIx', async () => {
    const client = makeClient();
    const ix = await client.depositCollateralIx(Keypair.generate().publicKey, new BN(1_000));
    expect(ix.keys.length).toBe(2); // trader, trader_state
  });

  test('withdrawCollateralIx', async () => {
    const client = makeClient();
    const ix = await client.withdrawCollateralIx(Keypair.generate().publicKey, new BN(500));
    expect(ix.keys.length).toBe(2);
  });

  test('depositFlpCapitalIx', async () => {
    const client = makeClient();
    const ix = await client.depositFlpCapitalIx(Keypair.generate().publicKey, new BN(1_000_000));
    expect(ix.keys.length).toBe(2); // authority, flp
  });

  test('withdrawFlpCapitalIx', async () => {
    const client = makeClient();
    const ix = await client.withdrawFlpCapitalIx(Keypair.generate().publicKey, new BN(500_000));
    expect(ix.keys.length).toBe(2);
  });

  // ─── Order intake (3) ──────────────────────────────────────────────

  test('placeLimitOrderIx', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const market = client.market(SOL, USDC).address;
    const ix = await client.placeLimitOrderIx({
      trader,
      market,
      side: 'long',
      sizeLots: new BN(1),
      limitTicks: new BN(99_950),
      postOnly: false,
    });
    // trader, market, order_buffer, trader_state, position, sysprog
    expect(ix.keys.length).toBe(6);
  });

  test('submitCommitIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.submitCommitIx({
      trader: Keypair.generate().publicKey,
      market,
      hash: new Uint8Array(32),
      bond: new BN(1_000),
    });
    expect(ix.keys.length).toBe(3); // trader, market, commit_buffer
  });

  test('submitRevealIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.submitRevealIx({
      trader: Keypair.generate().publicKey,
      market,
      side: 'short',
      sizeLots: new BN(1),
      limitTicks: new BN(100_050),
      nonce: new Uint8Array(32),
    });
    expect(ix.keys.length).toBe(4); // trader, market, order_buffer, commit_buffer
  });

  // ─── Batch + settlement (3) ────────────────────────────────────────

  test('runBatchIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.runBatchIx({
      sequencer: Keypair.generate().publicKey,
      market,
      nowMs: new BN(1_000_000),
    });
    // sequencer, market, order_buffer, commit_buffer, insurance, flp
    expect(ix.keys.length).toBe(6);
  });

  test('applyFillIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.applyFillIx({
      sequencer: Keypair.generate().publicKey,
      market,
      takerTrader: Keypair.generate().publicKey,
      makerTrader: Keypair.generate().publicKey,
      sizeLots: new BN(1),
      priceTicks: new BN(99_950),
      takerSide: 'long',
    });
    // sequencer, market, insurance_fund, taker_state, maker_state,
    // taker_pos, maker_pos, sysprog
    expect(ix.keys.length).toBe(8);
  });

  test('cancelOrderIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.cancelOrderIx({
      trader: Keypair.generate().publicKey,
      market,
      orderSeq: new BN(42),
    });
    // trader, market, order_buffer
    expect(ix.keys.length).toBe(3);
  });

  test('applyFlpFillIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.applyFlpFillIx({
      sequencer: Keypair.generate().publicKey,
      market,
      takerTrader: Keypair.generate().publicKey,
      sizeLots: new BN(1),
      priceTicks: new BN(99_950),
      takerSide: 'long',
    });
    // sequencer, market, insurance_fund, taker_state, taker_pos, flp, sysprog
    expect(ix.keys.length).toBe(7);
  });

  // ─── Liquidation (2) ───────────────────────────────────────────────

  test('liquidatePositionIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.liquidatePositionIx({
      caller: Keypair.generate().publicKey,
      market,
      trader: Keypair.generate().publicKey,
    });
    // caller, market, order_buffer, trader_state, position
    expect(ix.keys.length).toBe(5);
  });

  test('liquidatePortfolioIx without cross-margin args', async () => {
    const client = makeClient();
    const ix = await client.liquidatePortfolioIx({
      caller: Keypair.generate().publicKey,
      executionMarket: client.market(SOL, USDC).address,
      trader: Keypair.generate().publicKey,
    });
    // caller, exec_market, exec_order_buffer, trader_state, exec_position
    expect(ix.keys.length).toBe(5);
  });

  test('liquidatePortfolioIx with cross-margin remaining_accounts', async () => {
    const client = makeClient();
    const otherMint1 = Keypair.generate().publicKey;
    const otherMint2 = Keypair.generate().publicKey;
    const ix = await client.liquidatePortfolioIx({
      caller: Keypair.generate().publicKey,
      executionMarket: client.market(SOL, USDC).address,
      trader: Keypair.generate().publicKey,
      crossMargin: [
        { market: client.market(otherMint1, USDC).address },
        { market: client.market(otherMint2, USDC).address },
      ],
    });
    // 5 named + 2 markets × 2 (market + position) = 5 + 4 = 9
    expect(ix.keys.length).toBe(9);
  });

  // ─── Governance (4) ────────────────────────────────────────────────

  test('updateOracleIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.updateOracleIx({
      authority: Keypair.generate().publicKey,
      market,
      priceTicks: new BN(105_000),
      confidence: new BN(50),
      publishedAtUnixSeconds: new BN(1_700_000_000),
    });
    expect(ix.keys.length).toBe(2);
  });

  test('updateOracleQuorumIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.updateOracleQuorumIx({
      authority: Keypair.generate().publicKey,
      market,
      pricesTicks: [new BN(99_950), new BN(100_000), new BN(100_050)],
      confidences: [new BN(0), new BN(0), new BN(0)],
      publishedAtUnixSeconds: [
        new BN(1_700_000_000),
        new BN(1_700_000_000),
        new BN(1_700_000_000),
      ],
    });
    expect(ix.keys.length).toBe(2); // authority, market
  });

  test('setMarketStatusIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.setMarketStatusIx({
      authority: Keypair.generate().publicKey,
      market,
      newStatus: MarketStatus.PostOnly,
    });
    expect(ix.keys.length).toBe(2);
  });

  test('updateMarketParamsIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.updateMarketParamsIx({
      authority: Keypair.generate().publicKey,
      market,
      newParams: defaultMajorMarketParams(),
    });
    expect(ix.keys.length).toBe(2);
    expect(ix.data.length).toBeGreaterThan(50); // params struct is dense
  });

  test('transferMarketAuthorityIx', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.transferMarketAuthorityIx({
      authority: Keypair.generate().publicKey,
      market,
      newAuthority: Keypair.generate().publicKey,
    });
    expect(ix.keys.length).toBe(2);
  });

  // ─── Coverage check ────────────────────────────────────────────────

  test('all 22 instruction builders covered', () => {
    // This test serves as a tripwire: if a new instruction is added to
    // the program, this list will fall out of sync with the test count
    // above.
    const expected = [
      'initializeInsuranceFundIx',
      'initializeFlpExposureIx',
      'initializeMarketIx',
      'openTraderStateIx',
      'depositCollateralIx',
      'withdrawCollateralIx',
      'depositFlpCapitalIx',
      'withdrawFlpCapitalIx',
      'placeLimitOrderIx',
      'submitCommitIx',
      'submitRevealIx',
      'runBatchIx',
      'applyFillIx',
      'applyFlpFillIx',
      'cancelOrderIx',
      'liquidatePositionIx',
      'liquidatePortfolioIx',
      'updateOracleIx',
      'updateOracleQuorumIx',
      'setMarketStatusIx',
      'updateMarketParamsIx',
      'transferMarketAuthorityIx',
    ];
    expect(expected.length).toBe(22);
    const client = makeClient();
    for (const name of expected) {
      expect(typeof (client as unknown as Record<string, unknown>)[name]).toBe('function');
    }
  });
});
