// Thin Anchor client wrapper for the Flash Book program.
//
// `Program<Idl>` is the loose JSON-IDL form. For strongly-typed method
// builders, Anchor 0.30+ supports importing the IDL as a TypeScript file
// (via `anchor idl convert` or codegen). This scaffold uses the JSON form
// directly; the `methods` accessor below returns a loose record so each
// instruction builder can be invoked by name with runtime checking.

import {
  AnchorProvider,
  BorshAccountsCoder,
  Program,
  type Idl,
  type Wallet,
} from '@coral-xyz/anchor';
import { Connection, PublicKey, SystemProgram, type TransactionInstruction } from '@solana/web3.js';
import idlJson from '../idl.json' assert { type: 'json' };
import {
  commitBufferPda,
  flpExposurePda,
  insuranceFundPda,
  marketPda,
  orderBufferPda,
  positionPda,
  traderStatePda,
  FLASH_BOOK_PROGRAM_ID,
} from './pdas.ts';
import type { InsuranceFundInitParams, MarketParamsRaw } from './params.ts';

export const IDL = idlJson as unknown as Idl;

interface MethodsBuilder {
  accountsPartial: (accounts: Record<string, PublicKey>) => MethodsBuilder;
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

  // ─── Instruction builders ────────────────────────────────────────

  initializeInsuranceFundIx(
    authority: PublicKey,
    params: InsuranceFundInitParams,
  ): Promise<TransactionInstruction> {
    const fund = this.insuranceFund();
    return this.methods
      .initializeInsuranceFund(
        params.feeContributionBps,
        params.toxicityTaxContributionBps,
        params.liqPenaltyContributionBps,
        params.pauseThresholdQuoteLots,
      )
      .accountsPartial({
        authority,
        insuranceFund: fund.address,
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

  depositCollateralIx(trader: PublicKey, amount: bigint | number): Promise<TransactionInstruction> {
    const state = this.traderState(trader);
    return this.methods
      .depositCollateral(amount)
      .accountsPartial({
        trader,
        traderState: state.address,
      })
      .instruction();
  }

  withdrawCollateralIx(trader: PublicKey, amount: bigint | number): Promise<TransactionInstruction> {
    const state = this.traderState(trader);
    return this.methods
      .withdrawCollateral(amount)
      .accountsPartial({
        trader,
        traderState: state.address,
      })
      .instruction();
  }

  placeLimitOrderIx(args: {
    trader: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    sizeLots: bigint | number;
    limitTicks: bigint | number;
    postOnly?: boolean;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    const state = this.traderState(args.trader);
    const position = this.position(args.market, args.trader);
    return this.methods
      .placeLimitOrder(
        args.side === 'long' ? 0 : 1,
        args.sizeLots,
        args.limitTicks,
        args.postOnly ?? false,
      )
      .accountsPartial({
        trader: args.trader,
        market: args.market,
        orderBuffer: buffer.address,
        traderState: state.address,
        position: position.address,
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

  liquidatePositionIx(args: {
    caller: PublicKey;
    market: PublicKey;
    trader: PublicKey;
  }): Promise<TransactionInstruction> {
    const buffer = this.orderBuffer(args.market);
    const traderState = this.traderState(args.trader);
    const position = this.position(args.market, args.trader);
    return this.methods
      .liquidatePosition()
      .accountsPartial({
        caller: args.caller,
        market: args.market,
        orderBuffer: buffer.address,
        traderState: traderState.address,
        position: position.address,
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
    return this.methods
      .applyFlpFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
      )
      .accountsPartial({
        sequencer: args.sequencer,
        market: args.market,
        takerTraderState: takerState.address,
        takerPosition: takerPos.address,
        flpExposure: flp.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  applyFillIx(args: {
    sequencer: PublicKey;
    market: PublicKey;
    takerTrader: PublicKey;
    makerTrader: PublicKey;
    sizeLots: bigint | number;
    priceTicks: bigint | number;
    takerSide: 'long' | 'short';
  }): Promise<TransactionInstruction> {
    const takerState = this.traderState(args.takerTrader);
    const makerState = this.traderState(args.makerTrader);
    const takerPos = this.position(args.market, args.takerTrader);
    const makerPos = this.position(args.market, args.makerTrader);
    return this.methods
      .applyFill(
        args.sizeLots,
        args.priceTicks,
        args.takerSide === 'long' ? 0 : 1,
      )
      .accountsPartial({
        sequencer: args.sequencer,
        market: args.market,
        takerTraderState: takerState.address,
        makerTraderState: makerState.address,
        takerPosition: takerPos.address,
        makerPosition: makerPos.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  // ─── Decoders ────────────────────────────────────────────────────

  /** Hand-rolled accounts coder, useful in tests + indexers. */
  accountsCoder(): BorshAccountsCoder {
    return new BorshAccountsCoder(IDL);
  }
}
