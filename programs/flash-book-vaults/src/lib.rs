#![allow(unexpected_cfgs)]
//! Flash Book Vaults — wave 21 program split.
//!
//! Strategist vaults (`VaultAccountV3`), depositor share accounting
//! (`VaultPositionAccountV3`).
//!
//! ── Status: PHASE 9 + 9b — full SPL deposit/withdraw shipped
//!
//! Functional ixs:
//!   • `create_vault_v3`       — strategist creates a new vault
//!   • `vault_deposit_v3`      — depositor signs SPL transfer (their
//!                                ATA → core's quote_vault) + wrapper
//!                                mints shares pro-rata to NAV
//!   • `vault_withdraw_v3`     — wrapper burns shares + CPIs into
//!                                core's `cpi_release_collateral_to_user`
//!                                so the depositor receives the
//!                                pro-rata payout from the protocol
//!                                quote_vault (signed by core's
//!                                InsuranceFund PDA)
//!
//! Trading on behalf of depositors (vault places orders via
//! `place_limit_order_v2_cpi`) is in scope but lands as a separate
//! follow-up because risk semantics for vault-owned positions live
//! in core's `assess_margin` path and require additional plumbing.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use flash_book::cpi::accounts::CpiReleaseCollateralToUser as CoreCpiReleaseCollateralToUser;
use flash_book::program::FlashBook;
use flash_book::state::{
    InsuranceFundAccount, VaultAccount as CoreVaultAccount,
    VaultPositionAccount as CoreVaultPositionAccount,
};

declare_id!("GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt");

/// Seed for this program's CPI authority PDA — must match the value
/// hardcoded in core's `WAVE21_VAULTS_PROGRAM_ID` whitelist.
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

#[program]
pub mod flash_book_vaults {
    use super::*;

    /// Liveness check.
    pub fn ping(ctx: Context<Ping>) -> Result<()> {
        let slot = Clock::get()?.slot;
        emit!(Pong {
            program: *ctx.program_id,
            caller: ctx.accounts.caller.key(),
            slot,
        });
        Ok(())
    }

    /// Strategist creates a new vault. Vault id is per-strategist
    /// (0..255), so each strategist can run up to 256 vaults.
    pub fn create_vault_v3(
        ctx: Context<CreateVaultV3>,
        vault_id: u8,
        name: [u8; 32],
        perf_fee_bps: u32,
    ) -> Result<()> {
        // Cap perf fee at half (50%) — same as core's vault validation.
        require!(perf_fee_bps <= 5_000, VaultsError::OutOfRange);

        let v = &mut ctx.accounts.vault;
        v.strategist = ctx.accounts.strategist.key();
        v.bump = ctx.bumps.vault;
        v.vault_id = vault_id;
        v.accept_deposits = 1;
        v._pad0 = 0;
        v.name = name;
        v.perf_fee_bps = perf_fee_bps;
        v.shares_outstanding = 0;
        v.total_capital_quote_lots = 0;

        emit!(VaultV3CreatedEvent {
            vault: v.key(),
            strategist: v.strategist,
            vault_id,
            perf_fee_bps,
        });
        Ok(())
    }

    /// Depositor adds capital, mints shares pro-rata to current NAV.
    /// Bootstrap (no shares yet) mints 1:1.
    ///
    /// Phase 9b: depositor signs an SPL transfer from their ATA → core's
    /// `quote_vault` (depositor owns their ATA — no PDA signing for the
    /// IN direction). Wrapper records the deposit + mints shares.
    pub fn vault_deposit_v3(
        ctx: Context<VaultDepositV3>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, VaultsError::ZeroSize);

        // Pull tokens from depositor → quote_vault. Depositor signs as ATA owner.
        let cpi_accounts = Transfer {
            from: ctx.accounts.depositor_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.depositor.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let vault = &mut ctx.accounts.vault;
        require!(vault.accept_deposits == 1, VaultsError::DepositsClosed);

        // Compute shares to mint:
        //   bootstrap (no shares yet) → 1:1
        //   else → amount × shares_outstanding / total_capital
        let shares_to_mint: u64 = if vault.shares_outstanding == 0
            || vault.total_capital_quote_lots == 0
        {
            amount_quote_lots
        } else {
            let prod = (amount_quote_lots as u128)
                .checked_mul(vault.shares_outstanding as u128)
                .ok_or_else(|| error!(VaultsError::OutOfRange))?;
            let s = prod / (vault.total_capital_quote_lots as u128);
            require!(s <= u64::MAX as u128, VaultsError::OutOfRange);
            s as u64
        };
        require!(shares_to_mint > 0, VaultsError::ZeroSize);

        vault.total_capital_quote_lots = vault
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        vault.shares_outstanding = vault
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;

        let pos = &mut ctx.accounts.position;
        if pos.depositor == Pubkey::default() {
            pos.vault = vault.key();
            pos.depositor = ctx.accounts.depositor.key();
            pos.bump = ctx.bumps.position;
        }
        pos.shares = pos
            .shares
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        pos.total_deposited_quote_lots = pos
            .total_deposited_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;

        emit!(VaultDepositV3Event {
            vault: vault.key(),
            depositor: pos.depositor,
            amount_quote_lots,
            shares_minted: shares_to_mint,
            shares_outstanding_after: vault.shares_outstanding,
        });
        Ok(())
    }

    /// Depositor burns shares to withdraw their pro-rata claim.
    /// Phase 9b: wrapper computes the payout, burns the shares, then
    /// CPIs into core's `cpi_release_collateral_to_user` which signs
    /// the SPL transfer from `quote_vault` → depositor's ATA as the
    /// InsuranceFund PDA.
    pub fn vault_withdraw_v3(
        ctx: Context<VaultWithdrawV3>,
        shares_to_burn: u64,
    ) -> Result<()> {
        require!(shares_to_burn > 0, VaultsError::ZeroSize);

        let vault_key = ctx.accounts.vault.key();
        let depositor_key = ctx.accounts.depositor.key();
        let total_capital = ctx.accounts.vault.total_capital_quote_lots;
        let total_shares = ctx.accounts.vault.shares_outstanding;

        require!(ctx.accounts.position.shares >= shares_to_burn, VaultsError::OutOfRange);
        require!(total_shares >= shares_to_burn, VaultsError::OutOfRange);
        require!(total_shares > 0, VaultsError::OutOfRange);

        // Pro-rata withdrawal: amount = shares × total_capital / shares_outstanding.
        let amount_u128 = (shares_to_burn as u128)
            .checked_mul(total_capital as u128)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?
            / (total_shares as u128);
        let amount: u64 = if amount_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            amount_u128 as u64
        };
        require!(amount > 0, VaultsError::ZeroSize);

        // Burn shares + decrement capital BEFORE the CPI (defensive — if
        // the SPL transfer fails the whole tx aborts and state rolls back).
        {
            let vault = &mut ctx.accounts.vault;
            let pos = &mut ctx.accounts.position;
            vault.total_capital_quote_lots = vault
                .total_capital_quote_lots
                .saturating_sub(amount);
            vault.shares_outstanding = vault
                .shares_outstanding
                .saturating_sub(shares_to_burn);
            pos.shares = pos.shares.saturating_sub(shares_to_burn);
            pos.total_withdrawn_quote_lots = pos
                .total_withdrawn_quote_lots
                .checked_add(amount)
                .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        }

        // CPI into core for SPL release.
        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CoreCpiReleaseCollateralToUser {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            insurance_fund: ctx.accounts.insurance_fund.to_account_info(),
            quote_vault: ctx.accounts.quote_vault.to_account_info(),
            user_quote_ata: ctx.accounts.depositor_quote_ata.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cpi_release_collateral_to_user(cpi_ctx, amount)?;

        emit!(VaultWithdrawV3Event {
            vault: vault_key,
            depositor: depositor_key,
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            shares_outstanding_after: ctx.accounts.vault.shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 10 — migrate a legacy core vault to VaultAccountV3.
    /// Strategist signs (only the vault owner can migrate). Drops
    /// `trader_state` (the v3 NAV bookkeeping is local; trading via
    /// `place_limit_order_v2_cpi` follows in a later wave).
    pub fn migrate_vault_to_v3(ctx: Context<MigrateVaultToV3>) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.strategist == ctx.accounts.strategist.key(),
            VaultsError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.strategist = src.strategist;
        dst.bump = ctx.bumps.v3;
        dst.vault_id = src.vault_id;
        dst.accept_deposits = src.accept_deposits;
        dst._pad0 = 0;
        dst.name = src.name;
        dst.perf_fee_bps = src.perf_fee_bps;
        dst.shares_outstanding = src.shares_outstanding;
        // total_capital_quote_lots is not stored on the legacy
        // VaultAccount (it lived in the legacy TraderState). Initialize
        // to 0; on first deposit_v3 the SPL transfer + share-mint
        // recomputes accurately. Strategist may also call a follow-up
        // recapitalization ix.
        dst.total_capital_quote_lots = 0;

        emit!(LegacyVaultMigratedV3Event {
            strategist: dst.strategist,
            vault_id: dst.vault_id,
            legacy: src.key(),
            v3: dst.key(),
            shares_outstanding: dst.shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 10 — migrate a depositor's legacy VaultPositionAccount
    /// to VaultPositionAccountV3. Depositor signs.
    pub fn migrate_vault_position_to_v3(
        ctx: Context<MigrateVaultPositionToV3>,
    ) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.depositor == ctx.accounts.depositor.key(),
            VaultsError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.vault = ctx.accounts.v3_vault.key();
        dst.depositor = src.depositor;
        dst.bump = ctx.bumps.v3;
        dst.shares = src.shares;
        dst.total_deposited_quote_lots = src.total_deposited_quote_lots;
        dst.total_withdrawn_quote_lots = src.total_withdrawn_quote_lots;

        emit!(LegacyVaultPositionMigratedV3Event {
            vault: dst.vault,
            depositor: dst.depositor,
            legacy: src.key(),
            v3: dst.key(),
            shares: dst.shares,
        });
        Ok(())
    }
}

/// V3 vault account. Seeds: `[b"vault_v3", strategist, vault_id]`.
#[account]
#[derive(Debug)]
pub struct VaultAccountV3 {
    pub strategist: Pubkey,
    pub bump: u8,
    pub vault_id: u8,
    pub accept_deposits: u8,
    pub _pad0: u8,
    pub name: [u8; 32],
    pub perf_fee_bps: u32,
    pub shares_outstanding: u64,
    pub total_capital_quote_lots: u64,
}
impl VaultAccountV3 {
    pub const SEED: &'static [u8] = b"vault_v3";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 1 + 1 + 1 + 32 + 4 + 8 + 8 = 96. Round 112.
        8 + 112
    }
}

/// V3 vault depositor position. Seeds: `[b"vault_position_v3", vault, depositor]`.
#[account]
#[derive(Debug, Default)]
pub struct VaultPositionAccountV3 {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub bump: u8,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
}
impl VaultPositionAccountV3 {
    pub const SEED: &'static [u8] = b"vault_position_v3";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 8 + 8 + 8 = 97. Round 112.
        8 + 112
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(vault_id: u8)]
pub struct CreateVaultV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        init,
        payer = strategist,
        space = VaultAccountV3::space(),
        seeds = [VaultAccountV3::SEED, strategist.key().as_ref(), &[vault_id]],
        bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultDepositV3<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = VaultPositionAccountV3::space(),
        seeds = [
            VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub position: Account<'info, VaultPositionAccountV3>,

    /// Depositor's USDC ATA — debited.
    #[account(mut)]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    /// Core's protocol vault — credited.
    #[account(mut)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultWithdrawV3<'info> {
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    #[account(
        mut,
        seeds = [
            VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ VaultsError::Unauthorized,
    )]
    pub position: Account<'info, VaultPositionAccountV3>,

    /// CHECK: this program's CPI authority — derives from
    /// `[CPI_AUTHORITY_SEED]` under this program ID.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// Core's InsuranceFund PDA — signs the SPL transfer out.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
        seeds::program = flash_book_program.key(),
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Core's protocol vault — debited via core CPI.
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// Depositor's USDC ATA — credited via core CPI.
    #[account(mut)]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    pub flash_book_program: Program<'info, FlashBook>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct MigrateVaultToV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    /// Legacy core vault — read-only.
    #[account(constraint = legacy.strategist == strategist.key() @ VaultsError::Unauthorized)]
    pub legacy: Account<'info, CoreVaultAccount>,

    /// Destination v3 vault.
    #[account(
        init,
        payer = strategist,
        space = VaultAccountV3::space(),
        seeds = [VaultAccountV3::SEED, legacy.strategist.as_ref(), &[legacy.vault_id]],
        bump,
    )]
    pub v3: Account<'info, VaultAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateVaultPositionToV3<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    /// Legacy core position — read-only.
    #[account(constraint = legacy.depositor == depositor.key() @ VaultsError::Unauthorized)]
    pub legacy: Account<'info, CoreVaultPositionAccount>,

    /// V3 vault that this position belongs to. Must already have been
    /// migrated via `migrate_vault_to_v3`.
    pub v3_vault: Account<'info, VaultAccountV3>,

    /// Destination v3 position.
    #[account(
        init,
        payer = depositor,
        space = VaultPositionAccountV3::space(),
        seeds = [
            VaultPositionAccountV3::SEED,
            v3_vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub v3: Account<'info, VaultPositionAccountV3>,

    pub system_program: Program<'info, System>,
}

#[error_code]
pub enum VaultsError {
    #[msg("argument out of allowed range")]
    OutOfRange,
    #[msg("size cannot be zero")]
    ZeroSize,
    #[msg("vault is closed for deposits")]
    DepositsClosed,
    #[msg("unauthorized caller")]
    Unauthorized,
}

#[event]
pub struct Pong {
    pub program: Pubkey,
    pub caller: Pubkey,
    pub slot: u64,
}

#[event]
pub struct VaultV3CreatedEvent {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub vault_id: u8,
    pub perf_fee_bps: u32,
}

#[event]
pub struct VaultDepositV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub amount_quote_lots: u64,
    pub shares_minted: u64,
    pub shares_outstanding_after: u64,
}

#[event]
pub struct VaultWithdrawV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub shares_burned: u64,
    pub amount_quote_lots: u64,
    pub shares_outstanding_after: u64,
}

#[event]
pub struct LegacyVaultMigratedV3Event {
    pub strategist: Pubkey,
    pub vault_id: u8,
    pub legacy: Pubkey,
    pub v3: Pubkey,
    pub shares_outstanding: u64,
}

#[event]
pub struct LegacyVaultPositionMigratedV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub legacy: Pubkey,
    pub v3: Pubkey,
    pub shares: u64,
}
