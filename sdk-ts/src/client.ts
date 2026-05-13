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
  flpExposurePerMarketV3Pda,
  flpPositionV3Pda,
  insuranceFundPda,
  lpPositionPda,
  marketPda,
  positionPda,
  positionPdaLegacy,
  traderStatePda,
  traderSubAccountPda,
  triggerOrderPda,
  triggerOrderV3Pda,
  twapOrderPda,
  twapOrderV3Pda,
  icebergOrderPda,
  icebergOrderV3Pda,
  vaultPda,
  vaultPositionPda,
  vaultV3Pda,
  vaultPositionV3Pda,
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
  /**
   * Derive the Position PDA for a (market, trader, subIndex) tuple.
   *
   * Phase 2c: Position PDAs are keyed on the trader_state PDA, not on
   * the wallet. `subIndex = 0` (default) → main TraderState; `1..=255`
   * → a sub-account TraderState. Callers that already have a
   * trader_state PDA should use the lower-level {@link positionPda}
   * directly.
   */
  position(market: PublicKey, trader: PublicKey, subIndex: number = 0) {
    const traderStatePdaForLookup =
      subIndex === 0
        ? traderStatePda(trader, this.programId).address
        : traderSubAccountPda(trader, subIndex, this.programId).address;
    return positionPda(market, traderStatePdaForLookup, this.programId);
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

  /// Delegate the MarketAccount to the ER so the matcher running on
  /// the ER can mutate mark/funding/VPIN/current_batch in lockstep with
  /// the order book. Pair with `delegateMarketBookIx` — both delegations
  /// must be live for the matcher to run on the ER without forking
  /// state between layers.
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
    /**
     * Phase 2e — sub-account index this order belongs to. `0` (default)
     * targets the trader's main TraderState; `1..=255` targets a
     * pre-opened sub TraderState. The resting order remembers this so
     * ApplyFill routes fees + PnL to the right state when this order
     * is matched as a maker.
     */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .placeLimitOrderV2(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.flags ?? 0,
        args.expiresAtSlot ?? new BN(0),
        args.subIndex ?? 0,
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
    /**
     * Phase 2e — sub-account index. See {@link placeLimitOrderV2Ix}.
     * Emitted on `FillBatchEvent.taker_sub_index` so the sequencer can
     * route ApplyFill's `taker_trader_state` correctly.
     */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .placeTakerOrderV2(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.flags ?? 0,
        args.expiresAtSlot ?? new BN(0),
        args.subIndex ?? 0,
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

  /// V2 cancel-all: remove every resting order the trader owns in this
  /// market. Bounded by `MAX_CANCELS_PER_IX_V2 = 24` on-chain — traders
  /// with more open orders call this ix repeatedly until the emitted
  /// `BulkOrderCancelledV2Event.cancelled_count` returns 0.
  cancelAllV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    return this.methods
      .cancelAllV2()
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  /// V2 modify: atomic cancel + place. Cheaper than two separate txs
  /// (one signature, one set of account loads) and preserves the
  /// trader's intent across the cancel window. Validates the new params
  /// against the same gates as `placeLimitOrderV2Ix` BEFORE removing
  /// the old order, so a malformed modify never drops the original.
  modifyOrderV2Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    oldOrderId: bigint | BN;
    newSizeLots: bigint | number | BN;
    newLimitTicks: bigint | number | BN;
    newFlags?: number;
    newExpiresAtSlot?: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market);
    const sideU8 = args.side === 'long' ? 0 : 1;
    const oldIdBn =
      args.oldOrderId instanceof BN ? args.oldOrderId : new BN(args.oldOrderId.toString());
    return this.methods
      .modifyOrderV2(
        sideU8,
        oldIdBn,
        args.newSizeLots,
        args.newLimitTicks,
        args.newFlags ?? 0,
        args.newExpiresAtSlot ?? new BN(0),
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
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

  /// Open a sub-account for the trader at `subIndex` in 1..=255.
  /// sub_index = 0 is reserved for the legacy main account (use
  /// `openTraderStateIx`). The sub-account is a separate TraderState
  /// PDA — for Phase 1 it can only hold collateral (transferred via
  /// `transferMainToSubIx` / `transferSubToMainIx`). Phase 2 will
  /// enable trading directly from sub-accounts.
  openTraderSubAccountIx(args: {
    trader: PublicKey;
    subIndex: number;
  }): Promise<TransactionInstruction> {
    const sub = traderSubAccountPda(args.trader, args.subIndex, this.programId);
    return this.methods
      .openTraderSubAccount(args.subIndex)
      .accountsPartial({
        trader: args.trader,
        traderSubAccount: sub.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Move `amount` quote-lots from the trader's main TraderState to
  /// their sub-account at `subIndex`. Refuses if the main account has
  /// open positions (Phase 1 conservatism — Phase 2 lifts this once
  /// assess_margin is aware of sub-account collateral).
  transferMainToSubIx(args: {
    trader: PublicKey;
    subIndex: number;
    amount: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const main = this.traderState(args.trader);
    const sub = traderSubAccountPda(args.trader, args.subIndex, this.programId);
    return this.methods
      .transferMainToSub(args.subIndex, args.amount)
      .accountsPartial({
        trader: args.trader,
        mainTraderState: main.address,
        subTraderState: sub.address,
      })
      .instruction();
  }

  /// Mirror of `transferMainToSubIx` — moves collateral from the
  /// sub-account back to main. Refuses if the sub has open positions
  /// (won't be possible until Phase 2, but included for symmetry).
  transferSubToMainIx(args: {
    trader: PublicKey;
    subIndex: number;
    amount: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const main = this.traderState(args.trader);
    const sub = traderSubAccountPda(args.trader, args.subIndex, this.programId);
    return this.methods
      .transferSubToMain(args.subIndex, args.amount)
      .accountsPartial({
        trader: args.trader,
        mainTraderState: main.address,
        subTraderState: sub.address,
      })
      .instruction();
  }

  /// Phase 2 — switch a position to isolated margin. `amountQuoteLots`
  /// is reserved from the trader's pooled `TraderState.collateral_quote_lots`
  /// against this specific position; subsequent losses on the position
  /// (liquidation reward + health-check requirement) are bounded to the
  /// per-position bucket. The transition fails if the trader has
  /// another isolated position already (Phase 2 single-isolated cap).
  ///
  /// `otherPositions`: every OTHER position the trader holds with
  /// size_lots > 0, passed as `(market, position)` pairs in
  /// remainingAccounts so the post-transfer health check can stress
  /// the full cross set.
  setPositionIsolatedIx(args: {
    trader: PublicKey;
    market: PublicKey;
    amountQuoteLots: bigint | number | BN;
    otherPositions: Array<{ market: PublicKey; position: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const traderState = this.traderState(args.trader);
    const targetPosition = this.position(args.market, args.trader);
    const remaining = args.otherPositions.flatMap((p) => [
      { pubkey: p.market, isSigner: false, isWritable: false },
      { pubkey: p.position, isSigner: false, isWritable: false },
    ]);
    return this.methods
      .setPositionIsolated(args.amountQuoteLots)
      .accountsPartial({
        trader: args.trader,
        traderState: traderState.address,
        targetMarket: args.market,
        targetPosition: targetPosition.address,
      })
      .remainingAccounts(remaining)
      .instruction();
  }

  /// Phase 2 — switch a position back to cross margin. All of the
  /// per-position isolated collateral is returned to the trader's
  /// pooled `TraderState.collateral_quote_lots` and the resulting
  /// cross set must pass the standard assess_margin health check.
  setPositionCrossIx(args: {
    trader: PublicKey;
    market: PublicKey;
    otherPositions: Array<{ market: PublicKey; position: PublicKey }>;
  }): Promise<TransactionInstruction> {
    const traderState = this.traderState(args.trader);
    const targetPosition = this.position(args.market, args.trader);
    const remaining = args.otherPositions.flatMap((p) => [
      { pubkey: p.market, isSigner: false, isWritable: false },
      { pubkey: p.position, isSigner: false, isWritable: false },
    ]);
    return this.methods
      .setPositionCross()
      .accountsPartial({
        trader: args.trader,
        traderState: traderState.address,
        targetMarket: args.market,
        targetPosition: targetPosition.address,
      })
      .remainingAccounts(remaining)
      .instruction();
  }

  /// Phase 2c migration — move a Position from the LEGACY PDA
  /// `[POSITION_SEED, market, wallet]` to the new Phase 2c PDA
  /// `[POSITION_SEED, market, traderStatePda]`. One-shot per
  /// (wallet, market). The legacy position is closed and rent
  /// refunded to the trader. The trader's main TraderStateAccount
  /// is the receiving "address" — only main-account positions need
  /// to be migrated, because sub-account positions did not exist
  /// before Phase 2c.
  ///
  /// Use {@link positionPdaLegacy} from `@flash-book/sdk/pdas` to
  /// read the legacy PDA off-chain before migrating.
  migratePositionToTraderStateKeyIx(args: {
    trader: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const traderState = this.traderState(args.trader);
    const legacyPosition = positionPdaLegacy(args.market, args.trader, this.programId);
    const newPosition = positionPda(args.market, traderState.address, this.programId);
    return this.methods
      .migratePositionToTraderStateKey()
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        traderState: traderState.address,
        legacyPosition: legacyPosition.address,
        newPosition: newPosition.address,
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

  /// Authority-only: directly set the insurance fund's pause threshold
  /// (quote-lots). Raising the threshold above the current balance puts
  /// the protocol into the ADL-eligible state; lowering it relaxes the
  /// gate. Used by governance and by stress-test rigs that need to
  /// drive the protocol into ADL without first burning through actual
  /// insurance balance.
  setInsurancePauseThresholdIx(args: {
    authority: PublicKey;
    newThresholdQuoteLots: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    const arg =
      typeof args.newThresholdQuoteLots === 'object'
        ? args.newThresholdQuoteLots
        : new BN(args.newThresholdQuoteLots.toString());
    return this.methods
      .setInsurancePauseThreshold(arg)
      .accountsPartial({
        authority: args.authority,
        insuranceFund: fund.address,
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
   * V3 mark-engine: permissionless `settle_mark`. Snaps the market's
   * `markPriceTicks` to the freshly-attested oracle price. Anyone can
   * call (no authority) — the only gate is the per-market
   * `markSettleMinSlots` rate-limit. Oracle freshness + confidence are
   * re-checked inside the program (rejects with `OracleTooStale` /
   * `OracleConfidenceTooWide` if violated).
   *
   * Pair with on-chain `MarkPriceDriftEvent` to drive a permissionless
   * keeper that nudges the mark whenever it drifts off oracle.
   */
  settleMarkIx(args: {
    caller: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    return this.methods
      .settleMark()
      .accountsPartial({
        caller: args.caller,
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
    /// Phase 2i — sub-account index of the taker. Defaults to 0 (main).
    /// The on-chain handler re-derives the TraderState PDA from
    /// `(takerTrader, takerSubIndex)` and rejects with WrongTrader if
    /// the passed `takerTraderState` doesn't match. Pass the value
    /// emitted on `FillBatchEvent.taker_sub_index`.
    takerSubIndex?: number;
  }): Promise<TransactionInstruction> {
    const takerSubIndex = args.takerSubIndex ?? 0;
    const takerState = takerSubIndex === 0
      ? this.traderState(args.takerTrader)
      : traderSubAccountPda(args.takerTrader, takerSubIndex, this.programId);
    const takerPos = this.position(args.market, args.takerTrader, takerSubIndex);
    const flp = this.flpExposure();
    const fund = this.insuranceFund();
    return this.methods
      .applyFlpFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
        takerSubIndex,
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
    /// Phase 2i — sub-account index of the taker. Read from
    /// `FillBatchEvent.taker_sub_index`. Default 0 (main).
    takerSubIndex?: number;
    /// Phase 2i — sub-account index of the maker. Read from
    /// `FillEntry.maker_sub_index` on the matched resting order.
    /// Default 0 (main).
    makerSubIndex?: number;
  }): Promise<TransactionInstruction> {
    const takerSubIndex = args.takerSubIndex ?? 0;
    const makerSubIndex = args.makerSubIndex ?? 0;
    const takerState = takerSubIndex === 0
      ? this.traderState(args.takerTrader)
      : traderSubAccountPda(args.takerTrader, takerSubIndex, this.programId);
    const makerState = makerSubIndex === 0
      ? this.traderState(args.makerTrader)
      : traderSubAccountPda(args.makerTrader, makerSubIndex, this.programId);
    const takerPos = this.position(args.market, args.takerTrader, takerSubIndex);
    const makerPos = this.position(args.market, args.makerTrader, makerSubIndex);
    const fund = this.insuranceFund();
    return this.methods
      .applyFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
        args.takerWasJit ?? false,
        takerSubIndex,
        makerSubIndex,
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

  // ─── V3 monolithic ix builders ──────────────────────────────────────
  //
  // V3 ixs were originally in the (now-merged) flash-book-orders /
  // -vaults / -flp wrapper programs. After the wave-23 monolithic merge
  // they live under FLASH_BOOK_PROGRAM_ID. Seeds + semantics unchanged.

  /// PDA helpers (defaulting to FLASH_BOOK_PROGRAM_ID).
  triggerOrderV3(market: PublicKey, trader: PublicKey, triggerId: number) {
    return triggerOrderV3Pda(market, trader, triggerId, this.programId);
  }
  twapOrderV3(market: PublicKey, trader: PublicKey, twapId: number) {
    return twapOrderV3Pda(market, trader, twapId, this.programId);
  }
  icebergOrderV3(market: PublicKey, trader: PublicKey, icebergId: number) {
    return icebergOrderV3Pda(market, trader, icebergId, this.programId);
  }
  vaultV3(strategist: PublicKey, vaultId: number) {
    return vaultV3Pda(strategist, vaultId, this.programId);
  }
  vaultPositionV3(vault: PublicKey, depositor: PublicKey) {
    return vaultPositionV3Pda(vault, depositor, this.programId);
  }
  flpExposurePerMarketV3(market: PublicKey) {
    return flpExposurePerMarketV3Pda(market, this.programId);
  }
  flpPositionV3(exposure: PublicKey, lp: PublicKey) {
    return flpPositionV3Pda(exposure, lp, this.programId);
  }

  /// Create a v3 trigger order PDA.
  placeTriggerOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    triggerId: number;
    side: 'long' | 'short';
    /// 0 = fire on oracle ≤ trigger_price; 1 = fire on oracle ≥ trigger_price.
    kind: 0 | 1;
    sizeLots: bigint | number | BN;
    triggerPriceTicks: bigint | number | BN;
    limitPriceTicks: bigint | number | BN;
    reduceOnly?: boolean;
    expiresAtSlot?: bigint | number | BN;
    /**
     * Phase 2f — TraderState sub-account this trigger fires against.
     * Default 0 = main. The trigger remembers it and `execute_trigger_order_v3`
     * sets the synthetic RestingOrderV2.sub_index from this field so
     * the resulting fill routes to the right TraderState.
     */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const trig = this.triggerOrderV3(args.market, args.trader, args.triggerId);
    const sz = args.sizeLots instanceof BN ? args.sizeLots : new BN(args.sizeLots.toString());
    const tp = args.triggerPriceTicks instanceof BN
      ? args.triggerPriceTicks
      : new BN(args.triggerPriceTicks.toString());
    const lp = args.limitPriceTicks instanceof BN
      ? args.limitPriceTicks
      : new BN(args.limitPriceTicks.toString());
    const exp = args.expiresAtSlot === undefined
      ? new BN(0)
      : args.expiresAtSlot instanceof BN
        ? args.expiresAtSlot
        : new BN(args.expiresAtSlot.toString());
    return this.methods
      .placeTriggerOrderV3(
        args.triggerId,
        args.side === 'long' ? 0 : 1,
        args.kind,
        sz,
        tp,
        lp,
        args.reduceOnly ?? false,
        exp,
        args.subIndex ?? 0,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        triggerOrder: trig.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Execute a v3 trigger — permissionless caller.
  executeTriggerOrderV3Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    triggerOwner: PublicKey;
    triggerId: number;
  }): Promise<TransactionInstruction> {
    const trig = this.triggerOrderV3(args.market, args.triggerOwner, args.triggerId);
    const book = marketBookPda(args.market, this.programId);
    const pos = positionPda(args.market, args.triggerOwner, this.programId);
    return this.methods
      .executeTriggerOrderV3()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        triggerOrder: trig.address,
        position: pos.address,
      })
      .instruction();
  }

  /// Cancel a v3 trigger order.
  cancelTriggerOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    triggerId: number;
  }): Promise<TransactionInstruction> {
    const trig = this.triggerOrderV3(args.market, args.trader, args.triggerId);
    return this.methods
      .cancelTriggerOrderV3()
      .accountsPartial({
        trader: args.trader,
        triggerOrder: trig.address,
      })
      .instruction();
  }

  /// Create a v3 TWAP order PDA.
  placeTwapOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    twapId: number;
    side: 'long' | 'short';
    sliceSizeLots: bigint | number | BN;
    totalSizeLots: bigint | number | BN;
    limitPriceTicks: bigint | number | BN;
    slotInterval: bigint | number | BN;
    endSlot?: bigint | number | BN;
    /** Phase 2f — sub-account this TWAP's children belong to. */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const twap = this.twapOrderV3(args.market, args.trader, args.twapId);
    const bn = (v: bigint | number | BN) =>
      v instanceof BN ? v : new BN(v.toString());
    return this.methods
      .placeTwapOrderV3(
        args.twapId,
        args.side === 'long' ? 0 : 1,
        bn(args.sliceSizeLots),
        bn(args.totalSizeLots),
        bn(args.limitPriceTicks),
        bn(args.slotInterval),
        args.endSlot === undefined ? new BN(0) : bn(args.endSlot),
        args.subIndex ?? 0,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        twapOrder: twap.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Execute the next TWAP slice — permissionless caller.
  executeTwapSliceV3Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    twapId: number;
  }): Promise<TransactionInstruction> {
    const twap = this.twapOrderV3(args.market, args.trader, args.twapId);
    const book = marketBookPda(args.market, this.programId);
    return this.methods
      .executeTwapSliceV3()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        twapOrder: twap.address,
      })
      .instruction();
  }

  cancelTwapOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    twapId: number;
  }): Promise<TransactionInstruction> {
    const twap = this.twapOrderV3(args.market, args.trader, args.twapId);
    return this.methods
      .cancelTwapOrderV3()
      .accountsPartial({
        trader: args.trader,
        twapOrder: twap.address,
      })
      .instruction();
  }

  placeIcebergOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    icebergId: number;
    side: 'long' | 'short';
    totalSizeLots: bigint | number | BN;
    displayedSizeLots: bigint | number | BN;
    limitTicks: bigint | number | BN;
    expiresAtSlot?: bigint | number | BN;
    /** Phase 2f — sub-account this iceberg's children belong to. */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const ice = this.icebergOrderV3(args.market, args.trader, args.icebergId);
    const book = marketBookPda(args.market, this.programId);
    const bn = (v: bigint | number | BN) =>
      v instanceof BN ? v : new BN(v.toString());
    return this.methods
      .placeIcebergOrderV3(
        args.icebergId,
        args.side === 'long' ? 0 : 1,
        bn(args.totalSizeLots),
        bn(args.displayedSizeLots),
        bn(args.limitTicks),
        args.expiresAtSlot === undefined ? new BN(0) : bn(args.expiresAtSlot),
        args.subIndex ?? 0,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
        icebergOrder: ice.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  replenishIcebergV3Ix(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
    icebergId: number;
  }): Promise<TransactionInstruction> {
    const ice = this.icebergOrderV3(args.market, args.trader, args.icebergId);
    const book = marketBookPda(args.market, this.programId);
    return this.methods
      .replenishIcebergV3()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        marketBook: book.address,
        icebergOrder: ice.address,
      })
      .instruction();
  }

  cancelIcebergV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    icebergId: number;
  }): Promise<TransactionInstruction> {
    const ice = this.icebergOrderV3(args.market, args.trader, args.icebergId);
    return this.methods
      .cancelIcebergV3()
      .accountsPartial({
        trader: args.trader,
        icebergOrder: ice.address,
      })
      .instruction();
  }

  /// Atomic bracket: parent limit order + OCO TP/SL triggers, all in
  /// one tx.
  placeBracketOrderV3Ix(args: {
    trader: PublicKey;
    market: PublicKey;
    parentSide: 'long' | 'short';
    sizeLots: bigint | number | BN;
    parentLimitTicks: bigint | number | BN;
    tpTriggerId: number;
    tpTriggerPriceTicks: bigint | number | BN;
    tpLimitTicks: bigint | number | BN;
    slTriggerId: number;
    slTriggerPriceTicks: bigint | number | BN;
    slLimitTicks: bigint | number | BN;
    expiresAtSlot?: bigint | number | BN;
    /** Phase 2f — sub-account the parent order + both child triggers belong to. */
    subIndex?: number;
  }): Promise<TransactionInstruction> {
    const tp = this.triggerOrderV3(args.market, args.trader, args.tpTriggerId);
    const sl = this.triggerOrderV3(args.market, args.trader, args.slTriggerId);
    const book = marketBookPda(args.market, this.programId);
    const bn = (v: bigint | number | BN) =>
      v instanceof BN ? v : new BN(v.toString());
    return this.methods
      .placeBracketOrderV3(
        args.parentSide === 'long' ? 0 : 1,
        bn(args.sizeLots),
        bn(args.parentLimitTicks),
        args.tpTriggerId,
        bn(args.tpTriggerPriceTicks),
        bn(args.tpLimitTicks),
        args.slTriggerId,
        bn(args.slTriggerPriceTicks),
        bn(args.slLimitTicks),
        args.expiresAtSlot === undefined ? new BN(0) : bn(args.expiresAtSlot),
        args.subIndex ?? 0,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        marketBook: book.address,
        tpTrigger: tp.address,
        slTrigger: sl.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  // ─── Vaults v3 ─────────────────────────────────────────────────────

  createVaultV3Ix(args: {
    strategist: PublicKey;
    vaultId: number;
    name: Uint8Array;
    perfFeeBps: number;
  }): Promise<TransactionInstruction> {
    if (args.name.length !== 32) {
      throw new Error('vault name must be exactly 32 bytes');
    }
    const v = this.vaultV3(args.strategist, args.vaultId);
    return this.methods
      .createVaultV3(args.vaultId, Array.from(args.name), args.perfFeeBps)
      .accountsPartial({
        strategist: args.strategist,
        vault: v.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  vaultOpenTraderStateV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
  }): Promise<TransactionInstruction> {
    const ts = traderStatePda(args.vault, this.programId);
    return this.methods
      .vaultOpenTraderStateV3()
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        vaultTraderState: ts.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  vaultDepositV3Ix(args: {
    depositor: PublicKey;
    vault: PublicKey;
    amountQuoteLots: bigint | number | BN;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    depositorQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const pos = this.vaultPositionV3(args.vault, args.depositor);
    const ts = traderStatePda(args.vault, this.programId);
    const ata = args.depositorQuoteAta ?? associatedTokenAddress(args.depositor, args.quoteMint);
    const amount =
      args.amountQuoteLots instanceof BN
        ? args.amountQuoteLots
        : new BN(args.amountQuoteLots.toString());
    return this.methods
      .vaultDepositV3(amount)
      .accountsPartial({
        depositor: args.depositor,
        vault: args.vault,
        position: pos.address,
        depositorQuoteAta: ata,
        quoteVault: args.quoteVault,
        vaultTraderState: ts.address,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  vaultWithdrawV3Ix(args: {
    depositor: PublicKey;
    vault: PublicKey;
    sharesToBurn: bigint | number | BN;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    depositorQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const pos = this.vaultPositionV3(args.vault, args.depositor);
    const ts = traderStatePda(args.vault, this.programId);
    const fund = insuranceFundPda(this.programId);
    const ata = args.depositorQuoteAta ?? associatedTokenAddress(args.depositor, args.quoteMint);
    const shares =
      args.sharesToBurn instanceof BN
        ? args.sharesToBurn
        : new BN(args.sharesToBurn.toString());
    return this.methods
      .vaultWithdrawV3(shares)
      .accountsPartial({
        depositor: args.depositor,
        vault: args.vault,
        position: pos.address,
        insuranceFund: fund.address,
        quoteVault: args.quoteVault,
        depositorQuoteAta: ata,
        vaultTraderState: ts.address,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  vaultPlaceOrderV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number | BN;
    limitTicks: bigint | number | BN;
    flags?: number;
    expiresAtSlot?: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market, this.programId);
    const sz = args.sizeLots instanceof BN ? args.sizeLots : new BN(args.sizeLots.toString());
    const px = args.limitTicks instanceof BN ? args.limitTicks : new BN(args.limitTicks.toString());
    const exp =
      args.expiresAtSlot === undefined
        ? new BN(0)
        : args.expiresAtSlot instanceof BN
          ? args.expiresAtSlot
          : new BN(args.expiresAtSlot.toString());
    return this.methods
      .vaultPlaceOrderV3(args.side === 'long' ? 0 : 1, sz, px, args.flags ?? 0, exp)
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  vaultCancelOrderV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    orderId: bigint | BN;
  }): Promise<TransactionInstruction> {
    const book = marketBookPda(args.market, this.programId);
    const oid = args.orderId instanceof BN ? args.orderId : new BN(args.orderId.toString());
    return this.methods
      .vaultCancelOrderV3(args.side === 'long' ? 0 : 1, oid)
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        market: args.market,
        marketBook: book.address,
      })
      .instruction();
  }

  settleVaultPerfFeeV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
  }): Promise<TransactionInstruction> {
    const stratPos = this.vaultPositionV3(args.vault, args.strategist);
    const ts = traderStatePda(args.vault, this.programId);
    return this.methods
      .settleVaultPerfFeeV3()
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        strategistPosition: stratPos.address,
        vaultTraderState: ts.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  // ─── Per-market FLP v3 ─────────────────────────────────────────────

  initFlpPerMarketV3Ix(args: {
    authority: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    const exposure = this.flpExposurePerMarketV3(args.market);
    return this.methods
      .initFlpPerMarketV3()
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        exposure: exposure.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  recordFlpFillV3Ix(args: {
    authority: PublicKey;
    market: PublicKey;
    sizeLots: bigint | number | BN;
    priceTicks: bigint | number | BN;
    side: 'long' | 'short';
    realizedPnlDelta: bigint | number | BN;
  }): Promise<TransactionInstruction> {
    const exposure = this.flpExposurePerMarketV3(args.market);
    const bn = (v: bigint | number | BN) =>
      v instanceof BN ? v : new BN(v.toString());
    return this.methods
      .recordFlpFillV3(
        bn(args.sizeLots),
        bn(args.priceTicks),
        args.side === 'long' ? 0 : 1,
        bn(args.realizedPnlDelta),
      )
      .accountsPartial({
        authority: args.authority,
        exposure: exposure.address,
      })
      .instruction();
  }

  flpDepositV3Ix(args: {
    lp: PublicKey;
    market: PublicKey;
    amountQuoteLots: bigint | number | BN;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    lpQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const exposure = this.flpExposurePerMarketV3(args.market);
    const pos = this.flpPositionV3(exposure.address, args.lp);
    const ata = args.lpQuoteAta ?? associatedTokenAddress(args.lp, args.quoteMint);
    const amount =
      args.amountQuoteLots instanceof BN
        ? args.amountQuoteLots
        : new BN(args.amountQuoteLots.toString());
    return this.methods
      .flpDepositV3(amount)
      .accountsPartial({
        lp: args.lp,
        exposure: exposure.address,
        position: pos.address,
        lpQuoteAta: ata,
        quoteVault: args.quoteVault,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  flpWithdrawV3Ix(args: {
    lp: PublicKey;
    market: PublicKey;
    sharesToBurn: bigint | number | BN;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    lpQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const exposure = this.flpExposurePerMarketV3(args.market);
    const pos = this.flpPositionV3(exposure.address, args.lp);
    const fund = insuranceFundPda(this.programId);
    const ata = args.lpQuoteAta ?? associatedTokenAddress(args.lp, args.quoteMint);
    const shares =
      args.sharesToBurn instanceof BN
        ? args.sharesToBurn
        : new BN(args.sharesToBurn.toString());
    return this.methods
      .flpWithdrawV3(shares)
      .accountsPartial({
        lp: args.lp,
        exposure: exposure.address,
        position: pos.address,
        insuranceFund: fund.address,
        quoteVault: args.quoteVault,
        lpQuoteAta: ata,
        tokenProgram: TOKEN_PROGRAM_ID,
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

  // ─── JIT liquidation auctions ────────────────────────────────────

  /** Derive a JIT liquidation offer PDA. Seeds: jit_liq_offer | market | maker | nonce_le. */
  jitLiquidationOfferPda(market: PublicKey, maker: PublicKey, nonce: number): {
    address: PublicKey;
    bump: number;
  } {
    const nonceBytes = Buffer.alloc(4);
    nonceBytes.writeUInt32LE(nonce, 0);
    const [address, bump] = PublicKey.findProgramAddressSync(
      [Buffer.from('jit_liq_offer'), market.toBuffer(), maker.toBuffer(), nonceBytes],
      this.programId,
    );
    return { address, bump };
  }

  /// Maker pre-commits a tighter close offer for any (or a specific) underwater
  /// trader. When `liquidate_position_v2` fires, the matcher walks JIT offers
  /// before falling back to the synthetic `oracle ± liq_penalty` close.
  placeJitLiquidationOfferIx(args: {
    maker: PublicKey;
    market: PublicKey;
    nonce: number;
    targetTrader?: PublicKey;        // PublicKey.default = any trader
    side: 0 | 1;                      // 0 = close LONGs (buy), 1 = close SHORTs (sell)
    offerPriceTicks: bigint | BN;
    maxSizeLots: bigint | BN;
    expiresAtSlot?: bigint | BN;
    /** Phase 2f — maker's sub-account index. */
    makerSubIndex?: number;
  }): Promise<TransactionInstruction> {
    const jitOffer = this.jitLiquidationOfferPda(args.market, args.maker, args.nonce);
    const bn = (v: bigint | number | BN) =>
      typeof v === 'bigint' ? new BN(v.toString()) : v instanceof BN ? v : new BN(v);
    return this.methods
      .placeJitLiquidationOffer(
        args.nonce,
        args.targetTrader ?? PublicKey.default,
        args.side,
        bn(args.offerPriceTicks),
        bn(args.maxSizeLots),
        bn(args.expiresAtSlot ?? 0),
        args.makerSubIndex ?? 0,
      )
      .accountsPartial({
        maker: args.maker,
        market: args.market,
        jitOffer: jitOffer.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  cancelJitLiquidationOfferIx(args: {
    maker: PublicKey;
    jitOffer: PublicKey;
  }): Promise<TransactionInstruction> {
    return this.methods
      .cancelJitLiquidationOffer()
      .accountsPartial({
        maker: args.maker,
        jitOffer: args.jitOffer,
      })
      .instruction();
  }

  /// Migrate a pre-V3-mark-engine market account (1024 B body) to V3 (1152 B
  /// body). Reallocates the account, funds the rent diff from `authority`,
  /// and writes V3 defaults: mark_ema_alpha=2000bps, mark_max_change=500bps,
  /// mark_settle_min_slots=10, drift_alert=100bps. Idempotent.
  migrateMarketToV3Ix(args: {
    authority: PublicKey;
    market: PublicKey;
  }): Promise<TransactionInstruction> {
    return this.methods
      .migrateMarketToV3()
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  // ─── Pyth oracle integration ─────────────────────────────────────

  /** Derive the MarketOracleConfig PDA. Seeds: oracle_config | market. */
  marketOracleConfigPda(market: PublicKey): { address: PublicKey; bump: number } {
    const [address, bump] = PublicKey.findProgramAddressSync(
      [Buffer.from('oracle_config'), market.toBuffer()],
      this.programId,
    );
    return { address, bump };
  }

  /// Authority-only: install the Pyth feed binding for a market. After this,
  /// `updateOracleFromPyth` becomes callable by anyone.
  ///
  /// @param pythPriceFeedId 32-byte Pyth feed identifier (e.g. SOL/USD on mainnet)
  /// @param maxStalenessSeconds reject pulls older than this (e.g. 30)
  /// @param maxConfidenceBps reject pulls with conf/price > this in bps (e.g. 100 = 1%)
  /// @param tickDecimals scale factor for converting Pyth price → market ticks.
  ///                     With default tick=$0.001 and Pyth's typical -8 exponent, set to 3.
  initMarketOracleConfigIx(args: {
    authority: PublicKey;
    market: PublicKey;
    pythPriceFeedId: Buffer | Uint8Array | number[];
    maxStalenessSeconds: number;
    maxConfidenceBps: number;
    tickDecimals: number;
  }): Promise<TransactionInstruction> {
    const cfg = this.marketOracleConfigPda(args.market);
    const feedId: number[] = Array.isArray(args.pythPriceFeedId)
      ? args.pythPriceFeedId
      : Array.from(args.pythPriceFeedId as Uint8Array);
    if (feedId.length !== 32) {
      throw new Error(`pythPriceFeedId must be 32 bytes, got ${feedId.length}`);
    }
    return this.methods
      .initMarketOracleConfig(
        feedId,
        args.maxStalenessSeconds,
        args.maxConfidenceBps,
        args.tickDecimals,
      )
      .accountsPartial({
        authority: args.authority,
        market: args.market,
        oracleConfig: cfg.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Permissionless: pull a fresh price from a Pyth PriceUpdateV2 account into
  /// the market's oracle_* fields. The caller funds the tx; Pyth's account is
  /// the trust anchor. Validates feed_id, staleness, and confidence on-chain.
  ///
  /// @param priceUpdate the PriceUpdateV2 account (posted by Pyth Solana Receiver)
  updateOracleFromPythIx(args: {
    caller: PublicKey;
    market: PublicKey;
    priceUpdate: PublicKey;
  }): Promise<TransactionInstruction> {
    const cfg = this.marketOracleConfigPda(args.market);
    return this.methods
      .updateOracleFromPyth()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        oracleConfig: cfg.address,
        priceUpdate: args.priceUpdate,
      })
      .instruction();
  }

  // ─── Decoders ────────────────────────────────────────────────────

  /** Hand-rolled accounts coder, useful in tests + indexers. */
  accountsCoder(): BorshAccountsCoder {
    return new BorshAccountsCoder(IDL);
  }
}
