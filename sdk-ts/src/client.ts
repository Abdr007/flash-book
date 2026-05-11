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
  flpExposurePda,
  insuranceFundPda,
  lpPositionPda,
  marketPda,
  positionPda,
  traderStatePda,
  triggerOrderPda,
  twapOrderPda,
  icebergOrderPda,
  vaultPda,
  vaultPositionPda,
  marketBondPda,
  marketBookPda,
  marketLeverageTiersPda,
  feeTiersPda,
  MAGICBLOCK_DELEGATION_PROGRAM_ID,
  delegateBufferPda,
  delegationRecordPda,
  delegationMetadataPda,
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
        insuranceFund: fund.address,
        flpExposure: flp.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Initialize the v2 hypertree-backed orderbook for a market.
  /// Allocates a 9864-byte PDA at [b"market_book", market]. Wave-18
  /// foundation — the actual matcher migration happens in wave 18e.
  /// Once shipped, this will replace `initializeOrderBufferIx` + the
  /// legacy `OrderBufferAccount` (16-order cap forced by BPF stack);
  /// the v2 book carries 50+ orders/side and is realloc-extensible.
  initMarketBookIx(args: {
    authority: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .initMarketBook()
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        marketBook: book.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  // ─── MagicBlock ER delegation ─────────────────────────────────────────
  //
  // Lifecycle: init the v2 book on mainnet (initMarketBookIx), then
  // delegate BOTH the market_book PDA and the MarketAccount to the ER
  // (delegateMarketBookIx + delegateMarketIx). CLOB taker walks then
  // execute on the ER with sub-millisecond latency; the ER auto-commits
  // state back to mainnet at `commitFrequencyMs`. Undelegate at the
  // end-of-life of the ER instance.

  /// Delegate the v2 hypertree market_book to the MagicBlock ER.
  /// Production cadence target: commitFrequencyMs ≈ 50–200 ms.
  /// Pass `validator` to pin a specific ER validator or omit for
  /// permissionless selection.
  delegateMarketBookIx(args: {
    authority: PublicKey;
    market: PublicKey;
    commitFrequencyMs: number;
    validator?: PublicKey | null;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const buffer = delegateBufferPda(book.address, this.programId);
    const record = delegationRecordPda(book.address);
    const metadata = delegationMetadataPda(book.address);
    return this.methods
      .delegateMarketBook(args.commitFrequencyMs, args.validator ?? null)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        marketBook: book.address,
        ownerProgram: this.programId,
        delegateBuffer: buffer.address,
        delegationRecord: record.address,
        delegationMetadata: metadata.address,
        systemProgram: SystemProgram.programId,
        delegationProgram: MAGICBLOCK_DELEGATION_PROGRAM_ID,
      })
      .instruction();
  }

  /// Undelegate the market_book from the ER back to mainnet. State is
  /// flushed via the buffer PDA before control returns.
  undelegateMarketBookIx(args: {
    authority: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const buffer = delegateBufferPda(book.address, this.programId);
    return this.methods
      .undelegateMarketBook()
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        marketBook: book.address,
        ownerProgram: this.programId,
        delegateBuffer: buffer.address,
        systemProgram: SystemProgram.programId,
        delegationProgram: MAGICBLOCK_DELEGATION_PROGRAM_ID,
      })
      .instruction();
  }

  /// Delegate the MarketAccount to the ER. Required for run_batch_v2 to
  /// mutate mark/funding/VPIN/current_batch on the ER. Pair with
  /// delegateMarketBookIx — both delegations must be live for the
  /// matcher tick to run on the ER.
  delegateMarketIx(args: {
    authority: PublicKey;
    market: PublicKey;
    commitFrequencyMs: number;
    validator?: PublicKey | null;
  }): Promise<TransactionInstruction> {
    const buffer = delegateBufferPda(args.market, this.programId);
    const record = delegationRecordPda(args.market);
    const metadata = delegationMetadataPda(args.market);
    return this.methods
      .delegateMarket(args.commitFrequencyMs, args.validator ?? null)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        ownerProgram: this.programId,
        delegateBuffer: buffer.address,
        delegationRecord: record.address,
        delegationMetadata: metadata.address,
        systemProgram: SystemProgram.programId,
        delegationProgram: MAGICBLOCK_DELEGATION_PROGRAM_ID,
      })
      .instruction();
  }

  /// Undelegate the MarketAccount from the ER back to mainnet.
  undelegateMarketIx(args: {
    authority: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const buffer = delegateBufferPda(args.market, this.programId);
    return this.methods
      .undelegateMarket()
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        ownerProgram: this.programId,
        delegateBuffer: buffer.address,
        systemProgram: SystemProgram.programId,
        delegationProgram: MAGICBLOCK_DELEGATION_PROGRAM_ID,
      })
      .instruction();
  }

  /// V2 limit-order placement against the hypertree-backed orderbook.
  /// Runs ALONGSIDE the legacy `placeLimitOrderIx`. Validates intake
  /// (status, min lots, tick alignment, OI cap), computes a Phoenix-
  /// style order_id, and inserts into the bid or ask RBT inside the
  /// market_book account. No SPL token CPI on the hot path (free-funds
  /// settlement comes in wave 19).
  placeLimitOrderV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number;
    limitTicks: bigint | number;
    flags?: number;
    expiresAtSlot?: bigint | number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .placeLimitOrderV2(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.flags ?? 0,
        args.expiresAtSlot ?? new BN(0),
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  /// CLOB-style placement (Phoenix / Manifest semantics): IMMEDIATE
  /// matching against the opposite-side book at the maker's resting
  /// price (price-time priority), with residual inserted as resting.
  ///
  /// Flags (bit positions, OR-combine):
  ///   • bit 0  POST_ONLY    — reject if any matches (must rest)
  ///   • bit 1  REDUCE_ONLY  — only fills that reduce position
  ///   • bit 2  IOC          — cancel residual after walk (no rest)
  ///   • bit 3  JIT          — Drift-style JIT bonus
  ///   • bits 4-5  STP_MODE  — self-trade prevention mode
  ///   • bit 6  FOK          — fill-or-kill (revert if any residual)
  ///
  /// Use vs `placeLimitOrderV2Ix`:
  ///   • placeLimitOrderV2:    maker rest (post into the hypertree book)
  ///   • placeTakerOrderV2:    CLOB immediate match (low-latency)
  placeTakerOrderV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number;
    limitTicks: bigint | number;
    flags?: number;
    expiresAtSlot?: bigint | number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .placeTakerOrderV2(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.flags ?? 0,
        args.expiresAtSlot ?? new BN(0),
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  /// V2 read-side: emit the top-N levels of the hypertree-backed book as
  /// a `BookDepthV2Event`. Pure read — never mutates state. Walks the bid
  /// + ask RBTs in best-first order via the same iterators the wave-18f
  /// matcher consumes. Use this from off-chain tools to validate
  /// orderbook state without parsing the raw account bytes.
  viewBookDepthV2Ix(args: {
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .viewBookDepthV2()
      .accountsPartial({
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  /// V2 trigger execute — fires a stop-loss / take-profit / OCO trigger
  /// against the hypertree-backed book. Same trigger semantics as v1
  /// (kind, oracle compare, reduce-only, OCO partner deactivation); only
  /// the order injection target differs (v2 hypertree, not v1 buffer).
  /// Permissionless caller — pre-authorized by trader at trigger creation.
  ///
  /// Pass the OCO partner trigger PDA as the first remaining_account if
  /// this trigger participates in an OCO bracket.
  executeTriggerOrderV2Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    triggerOrder: PublicKey;
    triggerOwner: PublicKey;
    ocoPartner?: PublicKey;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const position = positionPda(args.market, args.triggerOwner);
    const builder = this.methods
      .executeTriggerOrderV2()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        triggerOrder: args.triggerOrder,
        position: position.address,
      });
    if (args.ocoPartner) {
      builder.remainingAccounts([
        { pubkey: args.ocoPartner, isWritable: true, isSigner: false },
      ]);
    }
    return builder.instruction();
  }

  /// V2 cancel: remove a resting order from the hypertree-backed book.
  /// `orderId` is the encoded Phoenix-style id from `OrderPlacedV2Event`
  /// (= `(price << 16) | (seq & 0xffff)`, inverted for bids). The SDK
  /// computes it via `encodeOrderIdV2(price, seq, side === 'long')`.
  cancelOrderV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    orderId: bigint | BN;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const sideU8 = args.side === 'long' ? 0 : 1;
    const orderIdBn =
      args.orderId instanceof BN ? args.orderId : new BN(args.orderId.toString());
    return this.methods
      .cancelOrderV2(sideU8, orderIdBn)
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  /// HIP-3 / permissionless market deployment. ANY signer can call this;
  /// they become BOTH the market authority and the creator (and earn
  /// `params.creatorShareBps` of net fee on every fill, forever). Params
  /// are clamped to a SAFE ENVELOPE on chain — see
  /// `permissionless_initialize_market` in lib.rs for the full clamp
  /// list (max_leverage ≤ 20×, fees in [10, 200] bps, maint margin ≥ 2%,
  /// per-trader notional ≤ 1% of FLP, etc.). Anything outside the
  /// envelope rejects with OutOfRange.
  permissionlessInitializeMarketIx(args: {
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
    const fund = this.insuranceFund();
    const flp = this.flpExposure();

    return this.methods
      .permissionlessInitializeMarket(args.params, args.initialOracleTicks)
      .accountsPartial({
        authority: args.authority,
        baseMint: args.baseMint,
        quoteMint: args.quoteMint,
        baseVault: args.baseVault,
        quoteVault: args.quoteVault,
        oracleAccount: args.oracleAccount,
        market: market.address,
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

  /// V2 2-leg basket order against the hypertree-backed book. Same
  /// validation + cross-market margin gate as v1 (place_basket_order).
  placeBasketOrderV2Ix(args: {
    trader: PublicKey;
    marketA: PublicKey;
    marketB: PublicKey;
    legA: { side: 'long' | 'short'; sizeLots: bigint | number; limitTicks: bigint | number; postOnly?: boolean };
    legB: { side: 'long' | 'short'; sizeLots: bigint | number; limitTicks: bigint | number; postOnly?: boolean };
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    const state = this.traderState(args.trader);
    const bookA = marketBookPda(args.marketA);
    const bookB = marketBookPda(args.marketB);
    const posA = this.position(args.marketA, args.trader);
    const posB = this.position(args.marketB, args.trader);
    const toLeg = (l: typeof args.legA) => ({
      side: l.side === 'long' ? 0 : 1,
      sizeLots: l.sizeLots,
      limitTicks: l.limitTicks,
      postOnly: l.postOnly ?? false,
    });
    return this.methods
      .placeBasketOrderV2(toLeg(args.legA), toLeg(args.legB))
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        flpExposure: flp.address,
        marketA: args.marketA,
        marketBookA: bookA.address,
        positionA: posA.address,
        marketB: args.marketB,
        marketBookB: bookB.address,
        positionB: posB.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// V2 N-leg basket order. remaining_accounts triples per leg are
  /// (market, market_book, position). Position PDAs must pre-exist.
  placeBasketOrderNV2Ix(args: {
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
    const remaining: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [];
    for (const leg of args.legs) {
      const book = marketBookPda(leg.market);
      const pos = this.position(leg.market, args.trader);
      remaining.push(
        { pubkey: leg.market, isWritable: false, isSigner: false },
        { pubkey: book.address, isWritable: true, isSigner: false },
        { pubkey: pos.address, isWritable: true, isSigner: false },
      );
    }
    return this.methods
      .placeBasketOrderNV2(ixLegs)
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

  /// HL-pattern partial withdrawal — pull collateral while positions
  /// remain open, gated by the safety floor `max(IM_required,
  /// 10% × total_notional)`. Pass every market the trader has a non-
  /// zero position in via `openPositionMarkets`; the on-chain handler
  /// walks (market, position) pairs in remaining_accounts.
  ///
  /// For traders with NO open positions, prefer `withdrawCollateralIx`
  /// (no remaining_accounts walk, smaller fee).
  partialWithdrawCollateralIx(args: {
    trader: PublicKey;
    amount: bigint | number;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    openPositionMarkets: ReadonlyArray<PublicKey>;
    traderQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    const fund = this.insuranceFund();
    const ata = args.traderQuoteAta ?? associatedTokenAddress(args.trader, args.quoteMint);
    const remaining: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [];
    for (const m of args.openPositionMarkets) {
      remaining.push(
        { pubkey: m, isWritable: false, isSigner: false },
        { pubkey: this.position(m, args.trader).address, isWritable: false, isSigner: false },
      );
    }
    const builder = this.methods
      .partialWithdrawCollateral(args.amount)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
        insuranceFund: fund.address,
        quoteMint: args.quoteMint,
        traderQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
      });
    if (remaining.length > 0) {
      const withRemaining = (builder as unknown as {
        remainingAccounts: (a: typeof remaining) => typeof builder;
      }).remainingAccounts(remaining);
      return withRemaining.instruction();
    }
    return builder.instruction();
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

  /// Initialize the per-market leverage-tier table (wave 20a, HL pattern).
  /// Authority-only. Tiers must be sorted ascending by minNotionalQuoteLots
  /// and each `mmrBps` must be ≥ market's baseline maintenance_margin_bps.
  initMarketLeverageTiersIx(args: {
    authority: PublicKey;
    market: PublicKey;
    tiers: ReadonlyArray<{ minNotionalQuoteLots: bigint | BN; mmrBps: number }>;
  }): Promise<TransactionInstruction> {
    const ZERO_PAD = [0, 0, 0, 0];
    const ixTiers = args.tiers.map((t) => ({
      minNotionalQuoteLots:
        t.minNotionalQuoteLots instanceof BN
          ? t.minNotionalQuoteLots
          : new BN(t.minNotionalQuoteLots.toString()),
      mmrBps: t.mmrBps,
      _pad: ZERO_PAD,
    }));
    const tiersPda = marketLeverageTiersPda(args.market, this.programId);
    return this.methods
      .initMarketLeverageTiers(ixTiers)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        leverageTiers: tiersPda.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Update an existing per-market leverage-tier table.
  updateMarketLeverageTiersIx(args: {
    authority: PublicKey;
    market: PublicKey;
    tiers: ReadonlyArray<{ minNotionalQuoteLots: bigint | BN; mmrBps: number }>;
  }): Promise<TransactionInstruction> {
    const ZERO_PAD = [0, 0, 0, 0];
    const ixTiers = args.tiers.map((t) => ({
      minNotionalQuoteLots:
        t.minNotionalQuoteLots instanceof BN
          ? t.minNotionalQuoteLots
          : new BN(t.minNotionalQuoteLots.toString()),
      mmrBps: t.mmrBps,
      _pad: ZERO_PAD,
    }));
    const tiersPda = marketLeverageTiersPda(args.market, this.programId);
    return this.methods
      .updateMarketLeverageTiers(ixTiers)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        leverageTiers: tiersPda.address,
      })
      .instruction();
  }

  // ─── Wave 22 — Multi-tier fee table (volume-based) ─────────────────

  /// Initialize the global fee tier table (HL/Binance/dYdX pattern).
  /// Authority signs. Tiers MUST be sorted ascending by
  /// `minVolumeQuoteLots`, the first tier MUST have `minVolume == 0`,
  /// and the schedule MUST be monotone improving (taker_fee ↘,
  /// maker_rebate ↗). All bps values within MAX_FEE_TIER_BPS = 1_000.
  /// `volumeWindowSlots` is the rolling-window length (HL: 14d ≈
  /// 3_024_000 slots @ 0.4s).
  ///
  /// Flash Trade can encode their existing tier schedule directly —
  /// the table is fully authority-parameterized, no fixed schema.
  initFeeTiersIx(args: {
    authority: PublicKey;
    volumeWindowSlots: bigint | BN;
    tiers: ReadonlyArray<{
      minVolumeQuoteLots: bigint | BN;
      makerRebateBps: number;
      takerFeeBps: number;
    }>;
  }): Promise<TransactionInstruction> {
    const ixTiers = args.tiers.map((t) => ({
      minVolumeQuoteLots:
        t.minVolumeQuoteLots instanceof BN
          ? t.minVolumeQuoteLots
          : new BN(t.minVolumeQuoteLots.toString()),
      makerRebateBps: t.makerRebateBps,
      takerFeeBps: t.takerFeeBps,
    }));
    const window =
      args.volumeWindowSlots instanceof BN
        ? args.volumeWindowSlots
        : new BN(args.volumeWindowSlots.toString());
    const ft = feeTiersPda(this.programId);
    return this.methods
      .initFeeTiers(window, ixTiers)
      .accountsPartial({
        authority: args.authority,
        feeTiers: ft.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Update the global fee tier table. Same validation as init.
  updateFeeTiersIx(args: {
    authority: PublicKey;
    volumeWindowSlots: bigint | BN;
    tiers: ReadonlyArray<{
      minVolumeQuoteLots: bigint | BN;
      makerRebateBps: number;
      takerFeeBps: number;
    }>;
  }): Promise<TransactionInstruction> {
    const ixTiers = args.tiers.map((t) => ({
      minVolumeQuoteLots:
        t.minVolumeQuoteLots instanceof BN
          ? t.minVolumeQuoteLots
          : new BN(t.minVolumeQuoteLots.toString()),
      makerRebateBps: t.makerRebateBps,
      takerFeeBps: t.takerFeeBps,
    }));
    const window =
      args.volumeWindowSlots instanceof BN
        ? args.volumeWindowSlots
        : new BN(args.volumeWindowSlots.toString());
    const ft = feeTiersPda(this.programId);
    return this.methods
      .updateFeeTiers(window, ixTiers)
      .accountsPartial({
        authority: args.authority,
        feeTiers: ft.address,
      })
      .instruction();
  }

  /// View ix — simulate to read the trader's effective tier
  /// (maker rebate + taker fee bps) given their current rolling-window
  /// volume. UIs surface this for "Your tier: VIP3 — 0.025% / 0.05%"
  /// display. Permissionless caller; trader pubkey passed as account.
  viewTraderEffectiveTierIx(args: {
    trader: PublicKey;
  }): Promise<TransactionInstruction> {
    const ts = this.traderState(args.trader);
    const ft = feeTiersPda(this.programId);
    return this.methods
      .viewTraderEffectiveTier()
      .accountsPartial({
        trader: args.trader,
        traderState: ts.address,
        feeTiers: ft.address,
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
  /// V2 cross-margin portfolio liquidation against the hypertree-backed
  /// book. Pure parity port of v1; only injection target differs.
  liquidatePortfolioV2Ix(args: {
    caller: PublicKey;
    executionMarket: PublicKey;
    trader: PublicKey;
    crossMargin?: ReadonlyArray<{ market: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.executionMarket);
    const traderState = this.traderState(args.trader);
    const position = this.position(args.executionMarket, args.trader);
    const builder = this.methods.liquidatePortfolioV2().accountsPartial({
      caller: args.caller,
      executionMarket: args.executionMarket,
      executionMarketBook: book.address,
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
      const withRemaining = (builder as unknown as {
        remainingAccounts: (a: typeof remaining) => typeof builder;
      }).remainingAccounts(remaining);
      return withRemaining.instruction();
    }
    return builder.instruction();
  }

  /// V2 liquidation — pure parity port of v1, just retargets the close
  /// order injection at the hypertree (order_type byte = 3, matcher
  /// promotes to OrderType::Liquidation FIFO priority). Same v1 maths
  /// (cooldown, stress lattice, Dutch auction reward, oracle ± penalty).
  /// Bonus: v2 ctx correctly marks `position` as mut (v1 has a latent
  /// bug — position writes silently don't persist).
  liquidatePositionV2Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    requestedCloseLots?: bigint | number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const traderState = this.traderState(args.trader);
    const callerState = this.traderState(args.caller);
    const position = this.position(args.market, args.trader);
    return this.methods
      .liquidatePositionV2(args.requestedCloseLots ?? new BN(0))
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
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
    /// WAVE 22 phase 2 (FLP path): when true, the global FeeTiersAccount
    /// is included so the TAKER's `taker_fee_bps` is resolved from
    /// their rolling-window volume against the tier table. FLP-side
    /// rebate stays flat (FLP is the protocol).
    useFeeTiers?: boolean;
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
        feeTiers: (args.useFeeTiers
          ? feeTiersPda(this.programId).address
          : null) as unknown as PublicKey,
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
    /// WAVE 22 phase 2: when true, the global FeeTiersAccount PDA is
    /// included so per-fill maker rebate / taker fee bps are resolved
    /// from the trader's rolling-window volume against the tier table.
    /// When false (or omitted), apply_fill falls back to flat
    /// `market.params.{maker_rebate_bps, taker_fee_bps}`.
    useFeeTiers?: boolean;
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
        // Anchor requires explicit `null` for optional accounts
        // (omission throws "Account not provided"). The cast is
        // needed because Anchor's IDL-derived TS type omits the
        // null union.
        feeTiers: (args.useFeeTiers
          ? feeTiersPda(this.programId).address
          : null) as unknown as PublicKey,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Ratchet a trailing-stop trigger order. Permissionless keeper ix —
  /// reads the oracle and updates the trigger price + anchor when the
  /// favorable-direction move is enough to change the tick-aligned
  /// trigger. No-op when the oracle hasn't moved past the anchor (or
  /// alignment doesn't change), so the keeper can call cheaply on every
  /// tick without burning fees on no-progress slots.
  updateTrailingStopIx(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    triggerId: number;
  }): Promise<TransactionInstruction> {
    const trigger = triggerOrderPda(args.market, args.trader, args.triggerId);
    return this.methods
      .updateTrailingStop()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        triggerOrder: trigger.address,
      })
      .instruction();
  }

  /// Cancel a trigger order — trader signs, account closes, rent
  /// returned. Works whether the trigger has fired (active=0) or not.
  cancelTriggerOrderIx(args: {
    trader: PublicKey;
    market: PublicKey;
    triggerId: number;
  }): Promise<TransactionInstruction> {
    const trigger = triggerOrderPda(args.market, args.trader, args.triggerId);
    return this.methods
      .cancelTriggerOrder()
      .accountsPartial({
        trader: args.trader,
        triggerOrder: trigger.address,
      })
      .instruction();
  }

  /// V2 TWAP slice — fires one slice against the hypertree-backed book.
  /// Same scheduling semantics as v1 (FLAG_ACTIVE / end_slot / slot_interval
  /// / slice sizing); only the order injection target differs (hypertree,
  /// not v1 buffer). Permissionless caller pays tx fee.
  executeTwapSliceV2Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    twapId: number;
  }): Promise<TransactionInstruction> {
    const twap = twapOrderPda(args.market, args.trader, args.twapId);
    const book = marketBookPda(args.market);
    return this.methods
      .executeTwapSliceV2()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        twapOrder: twap.address,
      })
      .instruction();
  }

  /// Cancel a TWAP order — trader signs, account closes, rent returned.
  cancelTwapOrderIx(args: {
    trader: PublicKey;
    market: PublicKey;
    twapId: number;
  }): Promise<TransactionInstruction> {
    const twap = twapOrderPda(args.market, args.trader, args.twapId);
    return this.methods
      .cancelTwapOrder()
      .accountsPartial({
        trader: args.trader,
        twapOrder: twap.address,
      })
      .instruction();
  }

  /// V2 iceberg replenish — fires the next visible chunk against the
  /// hypertree-backed book. Same iceberg semantics as v1 (still-resting
  /// probe via order_id lookup → no-op if prior chunk hasn't fully
  /// filled; auto-deactivate at zero remaining); only the order
  /// injection target differs (hypertree, not v1 buffer).
  replenishIcebergV2Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    icebergId: number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const ice = icebergOrderPda(args.market, args.trader, args.icebergId);
    return this.methods
      .replenishIcebergV2()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        icebergOrder: ice.address,
      })
      .instruction();
  }

  /// V2 cancel iceberg — O(log n) hypertree probe + remove (vs v1's O(n)
  /// buffer scan). Closes the iceberg account, refunds rent.
  cancelIcebergV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    icebergId: number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const ice = icebergOrderPda(args.market, args.trader, args.icebergId);
    return this.methods
      .cancelIcebergV2()
      .accountsPartial({
        trader: args.trader,
        marketBook: book.address,
        icebergOrder: ice.address,
      })
      .instruction();
  }

  /// View ix: predicted next-batch funding rate. Compose into a tx and
  /// `connection.simulateTransaction` to read the emit log without
  /// landing on chain. The PredictedFundingEvent in the logs carries
  /// (rate_bps_per_sec, premium_bps, current_cum_index).
  viewPredictedFundingIx(args: {
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    return this.methods
      .viewPredictedFunding()
      .accountsPartial({
        market: args.market,
        flpExposure: flp.address,
      })
      .instruction();
  }

  /// View ix: snapshot the FLP quoter's would-be next-batch quote
  /// ladder (top-level summary). Same simulation pattern as
  /// viewPredictedFundingIx — the QuoteLadderSnapshotEvent in the logs
  /// carries (fair_value, top bid/ask, top sizes, level_count). For the
  /// full per-level array, off-chain consumers can re-run
  /// `generate_quotes` with the same inputs (deterministic).
  viewQuoteLadderIx(args: {
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const flp = this.flpExposure();
    return this.methods
      .viewQuoteLadder()
      .accountsPartial({
        market: args.market,
        flpExposure: flp.address,
      })
      .instruction();
  }

  /// View ix: cross-market portfolio risk for a trader. Pass
  /// `[market, position]` pairs in `openPositions` — count must equal
  /// trader_state.open_positions. Pass `[]` if flat. SDK callers
  /// `simulateTransaction` and read the PortfolioRiskEvent from logs:
  /// (collateral, unrealized_pnl, equity, required_margin,
  /// health_ratio_bps, largest_position_market, largest_position_notional,
  /// open_positions, worst_scenario_idx). Authoritative — uses the
  /// same on-chain stress-lattice that liquidations do.
  viewPortfolioRiskIx(args: {
    trader: PublicKey;
    openPositions?: ReadonlyArray<{ market: PublicKey; position: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const ts = this.traderState(args.trader);
    const remaining = (args.openPositions ?? []).flatMap((p) => [
      { pubkey: p.market, isWritable: false, isSigner: false },
      { pubkey: p.position, isWritable: false, isSigner: false },
    ]);
    return this.methods
      .viewPortfolioRisk()
      .accountsPartial({
        traderState: ts.address,
      })
      .remainingAccounts(remaining)
      .instruction();
  }

  /// Set the per-position leverage cap (Hyperliquid pattern). `cap` ∈
  /// [1, market.maxLeverage]; 0 to clear. Trader OR delegate signs.
  /// Enforced on `placeLimitOrder` intake against projected post-fill
  /// notional. Existing oversize positions are NOT force-liquidated.
  setPositionLeverageIx(args: {
    authority: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    cap: number;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    const position = this.position(args.market, args.trader);
    return this.methods
      .setPositionLeverage(args.cap)
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        traderState: state.address,
        position: position.address,
      })
      .instruction();
  }

  /// Cross-margin sweep between two trader accounts under a common
  /// authority (master signs as delegate of both).
  ///
  /// Source can hold OPEN POSITIONS — pass `[market, position]` pairs in
  /// `openPositions` (count must equal source.open_positions). Post-
  /// sweep margin is evaluated against the joint stress-lattice; rejects
  /// if source becomes unhealthy. Source flat → pass `[]` (legacy fast
  /// path).
  sweepCollateralIx(args: {
    authority: PublicKey;
    fromTrader: PublicKey;
    toTrader: PublicKey;
    amountQuoteLots: bigint | number;
    openPositions?: ReadonlyArray<{ market: PublicKey; position: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const from = this.traderState(args.fromTrader);
    const to = this.traderState(args.toTrader);
    const remaining = (args.openPositions ?? []).flatMap((p) => [
      { pubkey: p.market, isWritable: false, isSigner: false },
      { pubkey: p.position, isWritable: false, isSigner: false },
    ]);
    return this.methods
      .sweepCollateral(args.amountQuoteLots)
      .accountsPartial({
        authority: args.authority,
        fromState: from.address,
        toState: to.address,
      })
      .remainingAccounts(remaining)
      .instruction();
  }

  /// AUTO-DELEVERAGE — permissionless safety primitive. When the
  /// insurance fund falls below `pause_threshold_quote_lots`, ADL
  /// keepers force-close the highest-ranked profitable counter-trader
  /// at the BANKRUPTCY price of an unhealthy position to absorb the
  /// loss. Caller should rank candidates off-chain by
  /// (unrealized_pnl × leverage) descending and submit the top one.
  /// Eligibility (counter profitable at bp + insurance below threshold
  /// + underwater is sick) is enforced on-chain.
  autoDeleverageIx(args: {
    caller: PublicKey;
    market: PublicKey;
    underwaterTrader: PublicKey;
    counterTrader: PublicKey;
    closeSizeLots: bigint | number;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const uts = this.traderState(args.underwaterTrader);
    const cts = this.traderState(args.counterTrader);
    const upos = this.position(args.market, args.underwaterTrader);
    const cpos = this.position(args.market, args.counterTrader);
    return this.methods
      .autoDeleverage(args.closeSizeLots)
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        insuranceFund: fund.address,
        underwaterTraderState: uts.address,
        underwaterPosition: upos.address,
        counterTraderState: cts.address,
        counterPosition: cpos.address,
      })
      .instruction();
  }

  /// Post a slashable HIP-3 deployer bond. Anyone can post bond on any
  /// market (per (market, depositor) pair). Bond is held in the protocol
  /// quote vault; slashable by governance. Adding to an existing bond
  /// cancels any pending unbond request.
  postMarketBondIx(args: {
    depositor: PublicKey;
    market: PublicKey;
    amountQuoteLots: bigint | number;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    return this.methods
      .postMarketBond(args.amountQuoteLots)
      .accountsPartial({
        depositor: args.depositor,
        market: args.market,
        insuranceFund: fund.address,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  /// Request to unbond a market bond. Sets the unbond timestamp; the
  /// bond becomes claimable after BOND_UNBOND_DELAY_SECONDS (7 days).
  /// Re-posting bond cancels the request.
  requestUnbondMarketBondIx(args: {
    depositor: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const bond = marketBondPda(args.market, args.depositor).address;
    return this.methods
      .requestUnbondMarketBond()
      .accountsPartial({
        depositor: args.depositor,
        marketBond: bond,
      })
      .instruction();
  }

  /// Claim unbonded market bond. Requires the unbond delay to have
  /// elapsed. Transfers the full outstanding amount back to the
  /// depositor's quote ATA. The MarketBondAccount stays open with
  /// amount=0 (re-postable).
  claimUnbondedMarketBondIx(args: {
    depositor: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const bond = marketBondPda(args.market, args.depositor).address;
    const fund = this.insuranceFund();
    return this.methods
      .claimUnbondedMarketBond()
      .accountsPartial({
        depositor: args.depositor,
        marketBond: bond,
        insuranceFund: fund.address,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  /// Slash a deployer bond. Protocol authority signs (single source of
  /// truth: insurance_fund.authority). Transfers `amount` from bond
  /// into insurance balance. Slash conditions are enforced off-chain
  /// by governance + monitors (oracle staleness, mass insolvency).
  slashMarketBondIx(args: {
    authority: PublicKey;
    market: PublicKey;
    bondDepositor: PublicKey;
    amountQuoteLots: bigint | number;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const bond = marketBondPda(args.market, args.bondDepositor).address;
    return this.methods
      .slashMarketBond(args.amountQuoteLots)
      .accountsPartial({
        authority: args.authority,
        insuranceFund: fund.address,
        marketBond: bond,
      })
      .instruction();
  }

  /// Crystallize the vault's high-water-mark performance fee. Strategist
  /// signs. Vault must be flat. If NAV/share has grown above the HWM,
  /// shares are minted to the strategist proportional to the gain ×
  /// perf_fee_bps; otherwise the call rejects.
  settleVaultPerfFeeIx(args: {
    strategist: PublicKey;
    vaultId: number;
  }): Promise<TransactionInstruction> {
    const vault = vaultPda(args.strategist, args.vaultId);
    const ts = this.traderState(vault.address);
    const sp = vaultPositionPda(vault.address, args.strategist);
    return this.methods
      .settleVaultPerfFee()
      .accountsPartial({
        strategist: args.strategist,
        vault: vault.address,
        vaultTraderState: ts.address,
        strategistPosition: sp.address,
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

  /// Approve, rotate, or revoke a builder code (Hyperliquid model).
  /// `builder` = the third-party UI/wallet/aggregator pubkey routing
  /// flow on the trader's behalf. `maxFeeShareBps` caps the share of
  /// net protocol fee the builder may collect — the on-chain emit
  /// clamps `min(market.builder_share_bps, maxFeeShareBps)`. Pass
  /// `PublicKey.default` to revoke (max share is forced to 0). Trader
  /// signs — only the user can install/rotate/revoke.
  setTraderBuilderIx(args: {
    trader: PublicKey;
    builder: PublicKey;
    maxFeeShareBps: number;
  }): Promise<TransactionInstruction> {
    const state = this.traderState(args.trader);
    return this.methods
      .setTraderBuilder(args.builder, args.maxFeeShareBps)
      .accountsPartial({
        trader: args.trader,
        traderState: state.address,
      })
      .instruction();
  }

  // ─── Decoders ────────────────────────────────────────────────────

  /** Hand-rolled accounts coder, useful in tests + indexers. */
  accountsCoder(): BorshAccountsCoder {
    return new BorshAccountsCoder(IDL);
  }
}
