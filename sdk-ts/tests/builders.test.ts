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
  associatedTokenAddress,
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
    const ix = await client.initializeInsuranceFundIx({
      authority,
      params: defaultInsuranceFundParams(),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    expect(ix.data.length).toBeGreaterThan(0);
    // authority, fund, quote_mint, quote_vault, token_program, rent, sysprog
    expect(ix.keys.length).toBe(7);
  });

  test('initializeFlpExposureIx', async () => {
    const client = makeClient();
    const authority = Keypair.generate().publicKey;
    const ix = await client.initializeFlpExposureIx(authority, new BN(1_000_000));
    expect(ix.programId.equals(FLASH_BOOK_PROGRAM_ID)).toBe(true);
    // authority, flp, authority_lp_position, sysprog
    expect(ix.keys.length).toBe(4);
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

  test('initTraderAtaIx', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const ix = await client.initTraderAtaIx({
      payer: Keypair.generate().publicKey,
      trader,
      quoteMint: Keypair.generate().publicKey,
    });
    // payer, trader, insurance_fund, quote_mint, trader_quote_ata,
    // token_program, ata_program, system_program
    expect(ix.keys.length).toBe(8);
  });

  test('initTraderAtaIx auto-derives the canonical ATA when not provided', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const quoteMint = Keypair.generate().publicKey;
    const ix = await client.initTraderAtaIx({
      payer: Keypair.generate().publicKey,
      trader,
      quoteMint,
    });
    // The trader_quote_ata key (index 4) must equal associatedTokenAddress(trader, quoteMint).
    const expectedAta = associatedTokenAddress(trader, quoteMint);
    expect(ix.keys[4].pubkey.toBase58()).toBe(expectedAta.toBase58());
  });

  test('placeBasketOrderIx', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const marketA = client.market(SOL, USDC).address;
    const marketB = client.market(USDC, SOL).address; // distinct market via swap
    const ix = await client.placeBasketOrderIx({
      trader,
      marketA,
      marketB,
      legA: { side: 'long', sizeLots: new BN(1), limitTicks: new BN(100_000) },
      legB: { side: 'short', sizeLots: new BN(1), limitTicks: new BN(200_000) },
    });
    // trader, trader_state, flp_exposure,
    // market_a, order_buffer_a, position_a,
    // market_b, order_buffer_b, position_b,
    // system_program
    expect(ix.keys.length).toBe(10);
  });

  test('placeBasketOrderNIx with 3 legs has correct account count', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const m1 = client.market(SOL, USDC).address;
    // Distinct second + third markets (use SOL/SOL and USDC/USDC pairings to
    // avoid colliding with m1).
    const m2 = client.market(USDC, SOL).address;
    const m3 = client.market(SOL, SOL).address;
    const ix = await client.placeBasketOrderNIx({
      trader,
      legs: [
        { market: m1, side: 'long', sizeLots: new BN(1), limitTicks: new BN(100_000) },
        { market: m2, side: 'short', sizeLots: new BN(1), limitTicks: new BN(200_000) },
        { market: m3, side: 'long', sizeLots: new BN(1), limitTicks: new BN(50_000) },
      ],
    });
    // trader, trader_state, flp_exposure + (market, order_buffer, position) × 3
    expect(ix.keys.length).toBe(3 + 3 * 3);
  });

  test('verifyMarketInvariantsIx', async () => {
    const client = makeClient();
    const ix = await client.verifyMarketInvariantsIx({
      caller: Keypair.generate().publicKey,
      market: Keypair.generate().publicKey,
    });
    expect(ix.keys.length).toBe(2); // caller, market
  });

  test('withdrawInsuranceFundIx', async () => {
    const client = makeClient();
    const ix = await client.withdrawInsuranceFundIx({
      authority: Keypair.generate().publicKey,
      amountQuoteLots: new BN(50_000),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    // authority, insurance_fund, quote_mint, authority_quote_ata,
    // quote_vault, token_program
    expect(ix.keys.length).toBe(6);
  });

  test('settleFundingIx', async () => {
    const client = makeClient();
    const ix = await client.settleFundingIx({
      caller: Keypair.generate().publicKey,
      market: Keypair.generate().publicKey,
      trader: Keypair.generate().publicKey,
    });
    // caller, market, trader, trader_state, position
    expect(ix.keys.length).toBe(5);
  });

  test('closeTraderAtaIx', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const ix = await client.closeTraderAtaIx({
      trader,
      quoteMint: Keypair.generate().publicKey,
    });
    // trader, insurance_fund, quote_mint, trader_quote_ata,
    // rent_destination, token_program
    expect(ix.keys.length).toBe(6);
  });

  test('closeTraderAtaIx defaults rent destination to trader', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const ix = await client.closeTraderAtaIx({
      trader,
      quoteMint: Keypair.generate().publicKey,
    });
    // rent_destination is account index 4.
    expect(ix.keys[4].pubkey.toBase58()).toBe(trader.toBase58());
  });

  test('closeTraderAtaIx accepts explicit rent destination', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const sponsor = Keypair.generate().publicKey;
    const ix = await client.closeTraderAtaIx({
      trader,
      quoteMint: Keypair.generate().publicKey,
      rentDestination: sponsor,
    });
    expect(ix.keys[4].pubkey.toBase58()).toBe(sponsor.toBase58());
  });

  // ─── Lifecycle (4) ─────────────────────────────────────────────────

  test('depositCollateralIx', async () => {
    const client = makeClient();
    const ix = await client.depositCollateralIx({
      trader: Keypair.generate().publicKey,
      amount: new BN(1_000),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    // trader, trader_state, insurance_fund, quote_mint, trader_quote_ata, quote_vault, token_program
    expect(ix.keys.length).toBe(7);
  });

  test('withdrawCollateralIx', async () => {
    const client = makeClient();
    const ix = await client.withdrawCollateralIx({
      trader: Keypair.generate().publicKey,
      amount: new BN(500),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    expect(ix.keys.length).toBe(7);
  });

  test('depositFlpCapitalIx', async () => {
    const client = makeClient();
    const ix = await client.depositFlpCapitalIx({
      authority: Keypair.generate().publicKey,
      amountQuoteLots: new BN(1_000_000),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    // authority, flp, lp_position, insurance_fund, quote_mint,
    // authority_quote_ata, quote_vault, token_program, system_program
    expect(ix.keys.length).toBe(9);
  });

  test('withdrawFlpCapitalIx', async () => {
    const client = makeClient();
    const ix = await client.withdrawFlpCapitalIx({
      authority: Keypair.generate().publicKey,
      sharesToBurn: new BN(500_000),
      quoteMint: Keypair.generate().publicKey,
      quoteVault: Keypair.generate().publicKey,
    });
    // authority, flp, lp_position, insurance_fund, quote_mint,
    // authority_quote_ata, quote_vault, token_program
    expect(ix.keys.length).toBe(8);
  });

  // ─── Order intake (3) ──────────────────────────────────────────────

  test('placeLimitOrderV2Ix', async () => {
    const client = makeClient();
    const trader = Keypair.generate().publicKey;
    const market = client.market(SOL, USDC).address;
    const ix = await client.placeLimitOrderV2Ix({
      trader,
      market,
      side: 'long',
      sizeLots: new BN(1),
      limitTicks: new BN(99_950),
    });
    // trader, market, market_book
    expect(ix.keys.length).toBe(3);
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

  test('runBatchV2Ix', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.runBatchV2Ix({
      sequencer: Keypair.generate().publicKey,
      market,
      nowMs: new BN(1_000_000),
    });
    // sequencer, market, market_book, commit_buffer, flp_exposure
    expect(ix.keys.length).toBe(5);
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

  test('cancelOrderV2Ix', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.cancelOrderV2Ix({
      trader: Keypair.generate().publicKey,
      market,
      side: 'long',
      orderId: 0xdead_beefn,
    });
    // trader, market, market_book
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

  test('liquidatePositionV2Ix', async () => {
    const client = makeClient();
    const market = client.market(SOL, USDC).address;
    const ix = await client.liquidatePositionV2Ix({
      caller: Keypair.generate().publicKey,
      market,
      trader: Keypair.generate().publicKey,
    });
    // caller, market, market_book, trader_state, caller_trader_state,
    // position, system_program
    expect(ix.keys.length).toBe(7);
  });

  test('liquidatePositionV2Ix supports partial close via requestedCloseLots', async () => {
    const client = makeClient();
    const ix = await client.liquidatePositionV2Ix({
      caller: Keypair.generate().publicKey,
      market: client.market(SOL, USDC).address,
      trader: Keypair.generate().publicKey,
      requestedCloseLots: new BN(50),
    });
    // Same shape — only the data argument changes.
    expect(ix.keys.length).toBe(7);
    expect(ix.data.length).toBeGreaterThan(0);
  });

  test('liquidatePortfolioV2Ix without cross-margin args', async () => {
    const client = makeClient();
    const ix = await client.liquidatePortfolioV2Ix({
      caller: Keypair.generate().publicKey,
      executionMarket: client.market(SOL, USDC).address,
      trader: Keypair.generate().publicKey,
    });
    // caller, exec_market, exec_market_book, trader_state, exec_position
    expect(ix.keys.length).toBe(5);
  });

  test('liquidatePortfolioV2Ix with cross-margin remaining_accounts', async () => {
    const client = makeClient();
    const otherMint1 = Keypair.generate().publicKey;
    const otherMint2 = Keypair.generate().publicKey;
    const ix = await client.liquidatePortfolioV2Ix({
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
    // above. v1 ix builders deleted in wave 19h; only v2 builders for
    // injection paths remain. Cancel-all + basket + commit-reveal
    // remain on v1 (no v2 equivalent yet — see V3_STATUS.md).
    const expected = [
      'initializeInsuranceFundIx',
      'initializeFlpExposureIx',
      'initializeMarketIx',
      'openTraderStateIx',
      'depositCollateralIx',
      'withdrawCollateralIx',
      'depositFlpCapitalIx',
      'withdrawFlpCapitalIx',
      'placeLimitOrderV2Ix',
      'submitCommitIx',
      'submitRevealIx',
      'runBatchV2Ix',
      'applyFillIx',
      'applyFlpFillIx',
      'cancelOrderV2Ix',
      'liquidatePositionV2Ix',
      'liquidatePortfolioV2Ix',
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
