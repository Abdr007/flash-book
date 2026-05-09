// Thin Anchor client wrapper for the Flash Book program.
//
// `Program<Idl>` is the loose JSON-IDL form. For strongly-typed method
// builders, Anchor 0.30+ supports importing the IDL as a TypeScript file
// (via `anchor idl convert` or codegen). This scaffold uses the JSON form
// directly; the `methods` accessor below returns a loose record so each
// instruction builder can be invoked by name with runtime checking.

import {
  AnchorProvider,
  BN,
  BorshAccountsCoder,
  Program,
  type Idl,
  type Wallet,
} from '@coral-xyz/anchor';
import {
  Connection,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  type TransactionInstruction,
} from '@solana/web3.js';
import idlJson from '../idl.json' assert { type: 'json' };
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  commitBufferPda,
  flpExposurePda,
  insuranceFundPda,
  lpPositionPda,
  marketPda,
  orderBufferPda,
  positionPda,
  traderStatePda,
  FLASH_BOOK_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
} from './pdas.ts';
import type { InsuranceFundInitParams, MarketParamsRaw } from './params.ts';

export const IDL = idlJson as unknown as Idl;
export { TOKEN_PROGRAM_ID, associatedTokenAddress };

/// Order-flag bits accepted by place_limit_order's `flags` argument.
/// Compose with bitwise OR. Reserved bits (3+) are rejected on chain.
export const ORDER_FLAG_POST_ONLY = 1 << 0;
export const ORDER_FLAG_REDUCE_ONLY = 1 << 1;
export const ORDER_FLAG_IOC = 1 << 2;
export const ORDER_FLAG_JIT = 1 << 3;

interface MethodsBuilder {
  accountsPartial: (accounts: Record<string, PublicKey>) => MethodsBuilder;
  remainingAccounts: (
    metas: ReadonlyArray<{ pubkey: PublicKey; isWritable: boolean; isSigner: boolean }>,
  ) => MethodsBuilder;
  instruction: () => Promise<TransactionInstruction>;
}

type MethodsRecord = Record<string, (...args: unknown[]) => MethodsBuilder>;

export class FlashBookClient {
  readonly program: Program<Idl>;
  readonly programId: PublicKey;

  constructor(
    public readonly connection: Connection,
    public readonly wallet: Wallet,
    programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
  ) {
    const provider = new AnchorProvider(connection, wallet, {
      commitment: 'confirmed',
      preflightCommitment: 'confirmed',
    });
    this.programId = programId;
    this.program = new Program<Idl>(IDL, provider);
  }

  private get methods(): MethodsRecord {
    return this.program.methods as unknown as MethodsRecord;
  }

  // ─── PDA helpers ─────────────────────────────────────────────────

  market(baseMint: PublicKey, quoteMint: PublicKey) {
    return marketPda(baseMint, quoteMint, this.programId);
  }
  orderBuffer(market: PublicKey) {
    return orderBufferPda(market, this.programId);
  }
  commitBuffer(market: PublicKey) {
    return commitBufferPda(market, this.programId);
  }
  insuranceFund() {
    return insuranceFundPda(this.programId);
  }
  flpExposure() {
    return flpExposurePda(this.programId);
  }
  traderState(trader: PublicKey) {
    return traderStatePda(trader, this.programId);
  }
  position(market: PublicKey, trader: PublicKey) {
    return positionPda(market, trader, this.programId);
  }
  lpPosition(lp: PublicKey) {
    return lpPositionPda(lp, this.programId);
  }

  // ─── Instruction builders ────────────────────────────────────────

  initializeFlpExposureIx(
    authority: PublicKey,
    initialCapitalQuoteLots: bigint | number,
  ): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const lpPos = this.lpPosition(authority);
    return this.methods
      .initializeFlpExposure(initialCapitalQuoteLots)
      .accountsPartial({
        authority,
        flpExposure: flp.address,
        authorityLpPosition: lpPos.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Deposit `amountQuoteLots` into the FLP pool. Mints LP shares at the
  /// prevailing NAV/share price (1:1 if pool empty, NAV-weighted otherwise).
  /// LpPositionAccount is created lazily via init_if_needed.
  depositFlpCapitalIx(args: {
    authority: PublicKey;
    amountQuoteLots: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    /** Optional override; defaults to the canonical ATA for (authority, quoteMint). */
    authorityQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const fund = this.insuranceFund();
    const lpPos = this.lpPosition(args.authority);
    const ata = args.authorityQuoteAta ?? associatedTokenAddress(args.authority, args.quoteMint);
    return this.methods
      .depositFlpCapital(args.amountQuoteLots)
      .accountsPartial({
        authority: args.authority,
        flpExposure: flp.address,
        lpPosition: lpPos.address,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        authorityQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Burn `sharesToBurn` LP shares and withdraw the proportional NAV claim.
  /// Caller must already own an LpPositionAccount with at least that many
  /// shares.
  withdrawFlpCapitalIx(args: {
    authority: PublicKey;
    sharesToBurn: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    authorityQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const fund = this.insuranceFund();
    const lpPos = this.lpPosition(args.authority);
    const ata = args.authorityQuoteAta ?? associatedTokenAddress(args.authority, args.quoteMint);
    return this.methods
      .withdrawFlpCapital(args.sharesToBurn)
      .accountsPartial({
        authority: args.authority,
        flpExposure: flp.address,
        lpPosition: lpPos.address,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        authorityQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  initializeInsuranceFundIx(args: {
    authority: PublicKey;
    params: InsuranceFundInitParams;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    return this.methods
      .initializeInsuranceFund(
        args.params.feeContributionBps,
        args.params.toxicityTaxContributionBps,
        args.params.liqPenaltyContributionBps,
        args.params.pauseThresholdQuoteLots,
      )
      .accountsPartial({
        authority: args.authority,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: SYSVAR_RENT_PUBKEY,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  initializeMarketIx(args: {
    authority: PublicKey;
    baseMint: PublicKey;
    quoteMint: PublicKey;
    baseVault: PublicKey;
    quoteVault: PublicKey;
    oracleAccount: PublicKey;
    params: MarketParamsRaw;
    initialOracleTicks: bigint | number;
  }): Promise<TransactionInstruction> {
    const market = this.market(args.baseMint, args.quoteMint);
    const orderBuffer = this.orderBuffer(market.address);
    const commitBuffer = this.commitBuffer(market.address);
    const fund = this.insuranceFund();
    const flp = this.flpExposure();

    return this.methods
      .initializeMarket(args.params, args.initialOracleTicks)
      .accountsPartial({
        authority: args.authority,
        baseMint: args.baseMint,
        quoteMint: args.quoteMint,
        baseVault: args.baseVault,
        quoteVault: args.quoteVault,
        oracleAccount: args.oracleAccount,
        market: market.address,
        orderBuffer: orderBuffer.address,
        commitBuffer: commitBuffer.address,
        insuranceFund: fund.address,
        flpExposure: flp.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  openTraderStateIx(trader: PublicKey): Promise<TransactionInstruction> {
    const state = this.traderState(trader);
    return this.methods
      .openTraderState()
      .accountsPartial({
        trader,
        traderState: state.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Authority-only: set a trader's per-trader fee discount in bps off
  /// the base taker fee. 0..10_000 (cap = 100% discount = zero fee).
  /// Universal CEX pattern (Binance, OKX, Bybit, Hyperliquid) — wired
  /// to off-chain 30-day rolling-volume tier tables.
  setTraderFeeTierIx(args: {
    authority: PublicKey;
    trader: PublicKey;
    discountBps: number;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const state = this.traderState(args.trader);
    return this.methods
      .setTraderFeeTier(args.discountBps)
      .accountsPartial({
        authority: args.authority,
        insuranceFund: fund.address,
        traderState: state.address,
      })
      .instruction();
  }

  /// Idempotently create the trader's quote ATA on-chain. Wraps a CPI to
  /// the AssociatedToken program via Anchor's `init_if_needed`. Safe to
  /// call repeatedly: existing ATAs are accepted as no-ops. The mint is
  /// constrained to `insurance_fund.quote_mint`, so this can only create
  /// ATAs that Flash Book recognizes for collateral.
  initTraderAtaIx(args: {
    payer: PublicKey;
    trader: PublicKey;
    quoteMint: PublicKey;
    /** Optional override; defaults to the canonical ATA for (trader, quoteMint). */
    traderQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const ata = args.traderQuoteAta ?? associatedTokenAddress(args.trader, args.quoteMint);
    return this.methods
      .initTraderAta()
      .accountsPartial({
        payer: args.payer,
        trader: args.trader,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        traderQuoteAta: ata,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Place two orders across two distinct markets atomically with a single
  /// cross-market stress-lattice gate. Hedged positions get correctly
  /// reduced required margin (the engine sees both legs' projected
  /// post-state at once). Atomic: any leg failure rolls back the whole tx.
  placeBasketOrderIx(args: {
    trader: PublicKey;
    marketA: PublicKey;
    marketB: PublicKey;
    legA: { side: 'long' | 'short'; sizeLots: bigint | number; limitTicks: bigint | number; postOnly?: boolean };
    legB: { side: 'long' | 'short'; sizeLots: bigint | number; limitTicks: bigint | number; postOnly?: boolean };
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const state = this.traderState(args.trader);
    const orderBufA = this.orderBuffer(args.marketA);
    const orderBufB = this.orderBuffer(args.marketB);
    const posA = this.position(args.marketA, args.trader);
    const posB = this.position(args.marketB, args.trader);
    const toLeg = (l: typeof args.legA) => ({
      side: l.side === 'long' ? 0 : 1,
      sizeLots: l.sizeLots,
      limitTicks: l.limitTicks,
      postOnly: l.postOnly ?? false,
    });
    return this.methods
      .placeBasketOrder(toLeg(args.legA), toLeg(args.legB))
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        flpExposure: flp.address,
        marketA: args.marketA,
        orderBufferA: orderBufA.address,
        positionA: posA.address,
        marketB: args.marketB,
        orderBufferB: orderBufB.address,
        positionB: posB.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// N-leg basket order (≤ MAX_BASKET_LEGS_N = 4). Generalises the
  /// fixed 2-leg `placeBasketOrderIx` via remaining_accounts walking.
  /// Position PDAs MUST already exist on each market — call a no-op
  /// place_limit_order first to init.
  placeBasketOrderNIx(args: {
    trader: PublicKey;
    legs: ReadonlyArray<{
      market: PublicKey;
      side: 'long' | 'short';
      sizeLots: bigint | number;
      limitTicks: bigint | number;
      postOnly?: boolean;
    }>;
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const state = this.traderState(args.trader);
    const ixLegs = args.legs.map((l) => ({
      side: l.side === 'long' ? 0 : 1,
      sizeLots: l.sizeLots,
      limitTicks: l.limitTicks,
      postOnly: l.postOnly ?? false,
    }));
    // Build remaining_accounts as triples [market, order_buffer, position]
    // per leg. Caller delivers the same array order as legs.
    const remaining: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [];
    for (const leg of args.legs) {
      const buf = this.orderBuffer(leg.market);
      const pos = this.position(leg.market, args.trader);
      remaining.push(
        { pubkey: leg.market, isWritable: false, isSigner: false },
        { pubkey: buf.address, isWritable: true, isSigner: false },
        { pubkey: pos.address, isWritable: true, isSigner: false },
      );
    }
    return this.methods
      .placeBasketOrderN(ixLegs)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        flpExposure: flp.address,
      })
      .remainingAccounts(remaining)
      .instruction();
  }

  /// Permissionlessly check market solvency invariants. Currently
  /// verifies open-interest balance (S5: oi_long == oi_short). On
  /// breach, the tx fails and an InvariantBreachDetectedEvent is
  /// emitted. Off-chain monitors should call this periodically and on
  /// breach trigger an explicit set_market_status(Paused) via the
  /// authority.
  verifyMarketInvariantsIx(args: {
    caller: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    return this.methods
      .verifyMarketInvariants()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
      })
      .instruction();
  }

  /// Authority withdraws excess insurance fund balance. Cannot push the
  /// fund below `pause_threshold_quote_lots`. Authority signs.
  withdrawInsuranceFundIx(args: {
    authority: PublicKey;
    amountQuoteLots: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    authorityQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const ata = args.authorityQuoteAta ?? associatedTokenAddress(args.authority, args.quoteMint);
    return this.methods
      .withdrawInsuranceFund(args.amountQuoteLots)
      .accountsPartial({
        authority: args.authority,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        authorityQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  /// Settle accrued funding for a single position. Permissionless — any
  /// signer can poke a position; `caller` pays the tx fee. The trader
  /// being settled doesn't need to sign. Idempotent: calling repeatedly
  /// is safe (delta=0 for an already-up-to-date position).
  settleFundingIx(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
  }): Promise<TransactionInstruction> {
    const traderState = this.traderState(args.trader);
    const position = this.position(args.market, args.trader);
    return this.methods
      .settleFunding()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        trader: args.trader,
        traderState: traderState.address,
        position: position.address,
      })
      .instruction();
  }

  /// Close the trader's quote ATA and refund rent to `rentDestination`
  /// (defaults to the trader). The trader signs as ATA authority. The
  /// SPL Token program enforces that the ATA must hold zero tokens —
  /// withdraw any remaining balance first.
  closeTraderAtaIx(args: {
    trader: PublicKey;
    quoteMint: PublicKey;
    /** Optional override; defaults to the trader. */
    rentDestination?: PublicKey;
    /** Optional override; defaults to the canonical ATA for (trader, quoteMint). */
    traderQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const ata = args.traderQuoteAta ?? associatedTokenAddress(args.trader, args.quoteMint);
    const dest = args.rentDestination ?? args.trader;
    return this.methods
      .closeTraderAta()
      .accountsPartial({
        trader: args.trader,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        traderQuoteAta: ata,
        rentDestination: dest,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  depositCollateralIx(args: {
    trader: PublicKey;
    amount: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    /** Optional override; defaults to the canonical ATA for (trader, quoteMint). */
    traderQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    const fund = this.insuranceFund();
    const ata = args.traderQuoteAta ?? associatedTokenAddress(args.trader, args.quoteMint);
    return this.methods
      .depositCollateral(args.amount)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        traderQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  withdrawCollateralIx(args: {
    trader: PublicKey;
    amount: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    traderQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    const fund = this.insuranceFund();
    const ata = args.traderQuoteAta ?? associatedTokenAddress(args.trader, args.quoteMint);
    return this.methods
      .withdrawCollateral(args.amount)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        traderQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  /// Place a limit order. Phoenix-grade order semantics via `flags`:
  ///   bit 0 — post_only (also via the explicit `postOnly` arg)
  ///   bit 1 — reduce_only: order can only shrink the trader's position
  ///   bit 2 — ioc: immediate-or-cancel; don't rest after batch
  ///   higher bits — reserved (chain rejects)
  placeLimitOrderIx(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number;
    limitTicks: bigint | number;
    postOnly?: boolean;
    /// Bitfield of flags. Use {ORDER_FLAG_*} constants.
    flags?: number;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    const state = this.traderState(args.trader);
    const position = this.position(args.market, args.trader);
    const flp = this.flpExposure();
    return this.methods
      .placeLimitOrder(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.postOnly ?? false,
        args.flags ?? 0,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        orderBuffer: buffer.address,
        traderState: state.address,
        position: position.address,
        flpExposure: flp.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  submitCommitIx(args: {
    trader: PublicKey;
    market: PublicKey;
    hash: Uint8Array;
    bond: bigint | number;
  }): Promise<TransactionInstruction> {
    const commitBuffer = this.commitBuffer(args.market);
    return this.methods
      .submitCommit(Array.from(args.hash), args.bond)
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        commitBuffer: commitBuffer.address,
      })
      .instruction();
  }

  submitRevealIx(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number;
    limitTicks: bigint | number;
    nonce: Uint8Array;
  }): Promise<TransactionInstruction> {
    const orderBuffer = this.orderBuffer(args.market);
    const commitBuffer = this.commitBuffer(args.market);
    return this.methods
      .submitReveal(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        Array.from(args.nonce),
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        orderBuffer: orderBuffer.address,
        commitBuffer: commitBuffer.address,
      })
      .instruction();
  }

  runBatchIx(args: {
    sequencer: PublicKey;
    market: PublicKey;
    nowMs: bigint | number;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    const commitBuffer = this.commitBuffer(args.market);
    const fund = this.insuranceFund();
    const flp = this.flpExposure();
    return this.methods
      .runBatch(args.nowMs)
      .accountsPartial({
        sequencer: args.sequencer,
        market: args.market,
        orderBuffer: buffer.address,
        commitBuffer: commitBuffer.address,
        insuranceFund: fund.address,
        flpExposure: flp.address,
      })
      .instruction();
  }

  setMarketStatusIx(args: {
    authority: PublicKey;
    market: PublicKey;
    newStatus: number;
  }): Promise<TransactionInstruction> {
    return this.methods
      .setMarketStatus(args.newStatus)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
      })
      .instruction();
  }

  updateMarketParamsIx(args: {
    authority: PublicKey;
    market: PublicKey;
    newParams: MarketParamsRaw;
  }): Promise<TransactionInstruction> {
    return this.methods
      .updateMarketParams(args.newParams)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
      })
      .instruction();
  }

  transferMarketAuthorityIx(args: {
    authority: PublicKey;
    market: PublicKey;
    newAuthority: PublicKey;
  }): Promise<TransactionInstruction> {
    return this.methods
      .transferMarketAuthority(args.newAuthority)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
      })
      .instruction();
  }

  updateOracleIx(args: {
    authority: PublicKey;
    market: PublicKey;
    priceTicks: bigint | number;
    confidence: bigint | number;
    publishedAtUnixSeconds: bigint | number;
  }): Promise<TransactionInstruction> {
    return this.methods
      .updateOracle(args.priceTicks, args.confidence, args.publishedAtUnixSeconds)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
      })
      .instruction();
  }

  /**
   * Multi-oracle quorum update. Pass three independent oracle observations
   * (price, confidence, publish_time). The matcher computes the median
   * price, validates dispersion, and writes conservative aggregates.
   *
   * In production, the caller is responsible for fetching each underlying
   * oracle account (e.g. Pyth, Switchboard, internal TWAP) and feeding
   * the values here. A future Phase 2 instruction will read directly via
   * CPI to remove the trust-the-caller assumption.
   */
  updateOracleQuorumIx(args: {
    authority: PublicKey;
    market: PublicKey;
    pricesTicks: [bigint | number, bigint | number, bigint | number];
    confidences: [bigint | number, bigint | number, bigint | number];
    publishedAtUnixSeconds: [bigint | number, bigint | number, bigint | number];
  }): Promise<TransactionInstruction> {
    return this.methods
      .updateOracleQuorum(
        args.pricesTicks,
        args.confidences,
        args.publishedAtUnixSeconds,
      )
      .accountsPartial({
        authority: args.authority,
        market: args.market,
      })
      .instruction();
  }

  /**
   * Cross-market portfolio liquidation. Walks the trader's positions
   * across multiple markets via remaining_accounts.
   *
   * `crossMargin` is the list of OTHER (market, position) pairs to
   * include in the cross-margin assessment alongside the execution
   * market's position. Each entry contributes one Market account
   * followed by one Position account to remaining_accounts.
   */
  liquidatePortfolioIx(args: {
    caller: PublicKey;
    executionMarket: PublicKey;
    trader: PublicKey;
    crossMargin?: ReadonlyArray<{ market: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.executionMarket);
    const traderState = this.traderState(args.trader);
    const position = this.position(args.executionMarket, args.trader);
    const builder = this.methods.liquidatePortfolio().accountsPartial({
      caller: args.caller,
      executionMarket: args.executionMarket,
      executionOrderBuffer: buffer.address,
      traderState: traderState.address,
      executionPosition: position.address,
    });
    const remaining: Array<{ pubkey: PublicKey; isWritable: boolean; isSigner: boolean }> = [];
    for (const m of args.crossMargin ?? []) {
      remaining.push({ pubkey: m.market, isWritable: false, isSigner: false });
      remaining.push({
        pubkey: this.position(m.market, args.trader).address,
        isWritable: false,
        isSigner: false,
      });
    }
    if (remaining.length > 0) {
      // anchor's MethodsBuilder shape is loose — cast and call .remainingAccounts.
      const withRemaining = (builder as unknown as {
        remainingAccounts: (a: typeof remaining) => typeof builder;
      }).remainingAccounts(remaining);
      return withRemaining.instruction();
    }
    return builder.instruction();
  }

  /// Liquidate an unhealthy position. Three production-grade behaviours:
  ///
  /// - `requestedCloseLots` = 0 → close the full position (legacy behaviour).
  ///   > 0 → partial liquidation (Hyperliquid-style); the chain validates
  ///   the size is ≤ position.size_lots.
  /// - When `market.params.liquidatorRewardBps > 0`, the caller receives
  ///   the reward credited to their own TraderState (auto-created on first
  ///   call via init_if_needed).
  /// - Race-safe: a second concurrent liquidator on the same position
  ///   gets LiquidationStale (position.size_lots == 0 after the first
  ///   tx commits).
  liquidatePositionIx(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    /// 0 = max (full close). Otherwise the on-chain handler closes
    /// exactly this many lots.
    requestedCloseLots?: bigint | number;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    const traderState = this.traderState(args.trader);
    const callerState = this.traderState(args.caller);
    const position = this.position(args.market, args.trader);
    return this.methods
      .liquidatePosition(args.requestedCloseLots ?? new BN(0))
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        orderBuffer: buffer.address,
        traderState: traderState.address,
        callerTraderState: callerState.address,
        position: position.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  applyFlpFillIx(args: {
    sequencer: PublicKey;
    market: PublicKey;
    takerTrader: PublicKey;
    sizeLots: bigint | number;
    priceTicks: bigint | number;
    takerSide: 'long' | 'short';
  }): Promise<TransactionInstruction> {
    const takerState = this.traderState(args.takerTrader);
    const takerPos = this.position(args.market, args.takerTrader);
    const flp = this.flpExposure();
    const fund = this.insuranceFund();
    return this.methods
      .applyFlpFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
      )
      .accountsPartial({
        sequencer: args.sequencer,
        market: args.market,
        insuranceFund: fund.address,
        takerTraderState: takerState.address,
        takerPosition: takerPos.address,
        flpExposure: flp.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// `takerWasJit`: set to true when the matched taker order was
  /// JIT-tagged (flag bit 3). Sequencer reads from the order's stored
  /// flags. When true, maker earns market.params.jit_bonus_rebate_bps
  /// extra rebate (Drift JIT incentive).
  applyFillIx(args: {
    sequencer: PublicKey;
    market: PublicKey;
    takerTrader: PublicKey;
    makerTrader: PublicKey;
    sizeLots: bigint | number;
    priceTicks: bigint | number;
    takerSide: 'long' | 'short';
    takerWasJit?: boolean;
  }): Promise<TransactionInstruction> {
    const takerState = this.traderState(args.takerTrader);
    const makerState = this.traderState(args.makerTrader);
    const takerPos = this.position(args.market, args.takerTrader);
    const makerPos = this.position(args.market, args.makerTrader);
    const fund = this.insuranceFund();
    return this.methods
      .applyFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
        args.takerWasJit ?? false,
      )
      .accountsPartial({
        sequencer: args.sequencer,
        market: args.market,
        insuranceFund: fund.address,
        takerTraderState: takerState.address,
        makerTraderState: makerState.address,
        takerPosition: takerPos.address,
        makerPosition: makerPos.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// One-time-write referrer. Hyperliquid affiliate model: while the
  /// taker has a referrer set, every fill emits a ReferralOwedEvent
  /// off-chain integrators credit. Cannot be rotated — anti-grief.
  setTraderReferrerIx(args: {
    trader: PublicKey;
    referrer: PublicKey;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    return this.methods
      .setTraderReferrer(args.referrer)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
      })
      .instruction();
  }

  /// Set or clear the trader's delegate authority. The trader signs;
  /// the delegate is the new pubkey allowed to act on the trader's
  /// behalf for trader-bound ix. Clear with PublicKey.default().
  setTraderDelegateIx(args: {
    trader: PublicKey;
    delegate: PublicKey;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    return this.methods
      .setTraderDelegate(args.delegate)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
      })
      .instruction();
  }

  cancelOrderIx(args: {
    trader: PublicKey;
    market: PublicKey;
    orderSeq: bigint | number;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    return this.methods
      .cancelOrder(args.orderSeq)
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        orderBuffer: buffer.address,
      })
      .instruction();
  }

  // ─── Decoders ────────────────────────────────────────────────────

  /** Hand-rolled accounts coder, useful in tests + indexers. */
  accountsCoder(): BorshAccountsCoder {
    return new BorshAccountsCoder(IDL);
  }
}
