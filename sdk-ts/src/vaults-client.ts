// FlashBookVaultsClient — TS builders for the wave-21 / wave-22 vault
// wrapper program (`flash_book_vaults`). Mirrors the FlashBookClient
// pattern. All ixs CPI into core (`flash_book`) for trading + SPL
// movement; the wrapper signs over its CPI authority PDA.

import {
  AnchorProvider,
  BN,
  Program,
  type Idl,
  type Wallet,
} from '@coral-xyz/anchor';
import {
  Connection,
  PublicKey,
  SystemProgram,
  type TransactionInstruction,
} from '@solana/web3.js';
import vaultsIdlJson from '../vaults-idl.json' assert { type: 'json' };
import {
  associatedTokenAddress,
  feeTiersPda,
  FLASH_BOOK_PROGRAM_ID,
  FLASH_BOOK_VAULTS_PROGRAM_ID,
  insuranceFundPda,
  marketBookPda,
  marketPda,
  TOKEN_PROGRAM_ID,
  traderStatePda,
  vaultPositionV3Pda,
  vaultV3Pda,
  wrapperCpiAuthorityPda,
} from './pdas.ts';

export const VAULTS_IDL = vaultsIdlJson as unknown as Idl;

interface MethodsBuilder {
  accountsPartial: (accounts: Record<string, PublicKey>) => MethodsBuilder;
  remainingAccounts: (
    metas: ReadonlyArray<{ pubkey: PublicKey; isWritable: boolean; isSigner: boolean }>,
  ) => MethodsBuilder;
  instruction: () => Promise<TransactionInstruction>;
}

type MethodsRecord = Record<string, (...args: unknown[]) => MethodsBuilder>;

export class FlashBookVaultsClient {
  readonly program: Program<Idl>;
  readonly programId: PublicKey;
  readonly coreProgramId: PublicKey;

  constructor(
    public readonly connection: Connection,
    public readonly wallet: Wallet,
    programId: PublicKey = FLASH_BOOK_VAULTS_PROGRAM_ID,
    coreProgramId: PublicKey = FLASH_BOOK_PROGRAM_ID,
  ) {
    const provider = new AnchorProvider(connection, wallet, {
      commitment: 'confirmed',
      preflightCommitment: 'confirmed',
    });
    this.programId = programId;
    this.coreProgramId = coreProgramId;
    this.program = new Program<Idl>(VAULTS_IDL, provider);
  }

  private get methods(): MethodsRecord {
    return this.program.methods as unknown as MethodsRecord;
  }

  // ─── PDA helpers ─────────────────────────────────────────────────

  vault(strategist: PublicKey, vaultId: number) {
    return vaultV3Pda(strategist, vaultId, this.programId);
  }
  vaultPosition(vault: PublicKey, depositor: PublicKey) {
    return vaultPositionV3Pda(vault, depositor, this.programId);
  }
  /// CPI authority PDA — derived under THIS program ID. Core's
  /// `cpi_release_collateral_to_user` / `place_limit_order_v2_cpi` /
  /// `cancel_order_v2_cpi` / etc. validate the signer matches one of
  /// the 3 wrapper-program authority derivations.
  cpiAuthority() {
    return wrapperCpiAuthorityPda(this.programId);
  }
  /// The vault PDA's TraderState in CORE's address space (seeded
  /// `[b"trader_state", vault_pda]` under flash_book_program). Used
  /// by `vault_open_trader_state_v3` and the deposit/withdraw paths.
  vaultTraderState(vault: PublicKey) {
    return traderStatePda(vault, this.coreProgramId);
  }

  // ─── Vault lifecycle ────────────────────────────────────────────

  /// Strategist creates a new vault. Vault id is per-strategist (0..255).
  /// `name` must be exactly 32 bytes (UTF-8, null-padded).
  /// `perfFeeBps` capped at 5_000 (50%).
  createVaultV3Ix(args: {
    strategist: PublicKey;
    vaultId: number;
    name: Uint8Array;
    perfFeeBps: number;
  }): Promise<TransactionInstruction> {
    if (args.name.length !== 32) {
      throw new Error('vault name must be exactly 32 bytes');
    }
    const v = this.vault(args.strategist, args.vaultId);
    return this.methods
      .createVaultV3(args.vaultId, Array.from(args.name), args.perfFeeBps)
      .accountsPartial({
        strategist: args.strategist,
        vault: v.address,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Bootstrap the vault PDA's TraderState in CORE so the vault can
  /// trade. Strategist signs (and pays rent). One-time setup, must be
  /// called BEFORE the first `vaultDepositV3Ix` (deposit credits the
  /// core TraderState collateral).
  vaultOpenTraderStateV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
  }): Promise<TransactionInstruction> {
    const cpiAuth = this.cpiAuthority();
    const ts = this.vaultTraderState(args.vault);
    return this.methods
      .vaultOpenTraderStateV3()
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        cpiAuthority: cpiAuth.address,
        vaultTraderState: ts.address,
        flashBookProgram: this.coreProgramId,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Deposit USDC into the vault. SPL transfer depositor → core's
  /// quote_vault, wrapper mints shares pro-rata, AND credits the
  /// vault's core TraderState collateral via inverse CPI (so the
  /// matcher recognizes the trading capital).
  vaultDepositV3Ix(args: {
    depositor: PublicKey;
    vault: PublicKey;
    amountQuoteLots: bigint | number | BN;
    quoteMint: PublicKey;
    /// Core's protocol vault TokenAccount — fetch via
    /// `fetchInsuranceFund(...).quoteVault` (one-time read).
    quoteVault: PublicKey;
    /// Optional override; defaults to canonical ATA(depositor, quoteMint).
    depositorQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const pos = this.vaultPosition(args.vault, args.depositor);
    const cpiAuth = this.cpiAuthority();
    const ts = this.vaultTraderState(args.vault);
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
        cpiAuthority: cpiAuth.address,
        vaultTraderState: ts.address,
        flashBookProgram: this.coreProgramId,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /// Burn shares for pro-rata payout. Wrapper debits core TraderState
  /// collateral, then CPIs core for SPL release (signed as
  /// InsuranceFund PDA).
  vaultWithdrawV3Ix(args: {
    depositor: PublicKey;
    vault: PublicKey;
    sharesToBurn: bigint | number | BN;
    quoteMint: PublicKey;
    quoteVault: PublicKey;
    depositorQuoteAta?: PublicKey;
  }): Promise<TransactionInstruction> {
    const pos = this.vaultPosition(args.vault, args.depositor);
    const cpiAuth = this.cpiAuthority();
    const ts = this.vaultTraderState(args.vault);
    const fund = insuranceFundPda(this.coreProgramId);
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
        cpiAuthority: cpiAuth.address,
        insuranceFund: fund.address,
        quoteVault: args.quoteVault,
        depositorQuoteAta: ata,
        vaultTraderState: ts.address,
        flashBookProgram: this.coreProgramId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  // ─── Vault trading (wave 22 phase 5) ────────────────────────────

  /// Strategist places a limit order on behalf of vault depositors.
  /// Wrapper signs CPI to core's place_limit_order_v2_cpi with vault
  /// PDA as the trader.
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
    const cpiAuth = this.cpiAuthority();
    const book = marketBookPda(args.market, this.coreProgramId);
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
        cpiAuthority: cpiAuth.address,
        market: args.market,
        marketBook: book.address,
        flashBookProgram: this.coreProgramId,
      })
      .instruction();
  }

  /// Strategist cancels a vault-PDA order. CPIs core's
  /// cancel_order_v2_cpi which validates ownership (resting order
  /// trader == vault PDA).
  vaultCancelOrderV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
    market: PublicKey;
    side: 'long' | 'short';
    orderId: bigint | BN;
  }): Promise<TransactionInstruction> {
    const cpiAuth = this.cpiAuthority();
    const book = marketBookPda(args.market, this.coreProgramId);
    const oid = args.orderId instanceof BN ? args.orderId : new BN(args.orderId.toString());
    return this.methods
      .vaultCancelOrderV3(args.side === 'long' ? 0 : 1, oid)
      .accountsPartial({
        strategist: args.strategist,
        vault: args.vault,
        cpiAuthority: cpiAuth.address,
        market: args.market,
        marketBook: book.address,
        flashBookProgram: this.coreProgramId,
      })
      .instruction();
  }

  // ─── Wave 22 phase 4 — perf fee crystallization ──────────────────

  /// Strategist crystallizes the performance fee. Mints perf-shares
  /// to strategist's position when current NAV/share > HWM.
  /// Bootstrap (HWM=0) anchors at then-current NAV/share.
  /// Below-HWM rejects.
  settleVaultPerfFeeV3Ix(args: {
    strategist: PublicKey;
    vault: PublicKey;
  }): Promise<TransactionInstruction> {
    const stratPos = this.vaultPosition(args.vault, args.strategist);
    const ts = this.vaultTraderState(args.vault);
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

}

// Suppress unused — kept available so consumers importing the
// vaults-client surface get these PDA helpers if they need them.
void feeTiersPda;
void marketPda;
void insuranceFundPda;
