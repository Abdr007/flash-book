#![allow(unexpected_cfgs)]
//! Flash Book Vaults — wave 21 program split.
//!
//! Strategist vaults (`VaultAccountV3`), depositor share accounting
//! (`VaultPositionAccountV3`). The vault's actual TRADING (CPI into
//! core's `place_limit_order_v2_cpi` to place orders on behalf of
//! depositors) ships in phase 9b once the vault collateral PDA is
//! wired through core's apply_fill flow.
//!
//! ── Status: PHASE 9 — account types + create/deposit/withdraw shipped
//!
//! Functional ixs:
//!   • `create_vault_v3`       — strategist creates a new vault
//!   • `vault_deposit_v3`      — depositor adds capital (mints shares
//!                                pro-rata to NAV)
//!   • `vault_withdraw_v3`     — depositor burns shares (burns pro-rata
//!                                to NAV; capital flows back via SPL
//!                                transfer in phase 9b once core's
//!                                inverse-CPI is wired)
//!
//! NAV bookkeeping is done locally in this program (shares_outstanding +
//! total_capital_quote_lots tracked here). The actual SPL transfer
//! between depositor's ATA and the vault's collateral PDA stays in
//! core (phase 9b) for the same auth-routing reason as FLP.

use anchor_lang::prelude::*;

declare_id!("GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt");

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
    /// Note: this ix only updates SHARE accounting locally; the SPL
    /// token transfer of `amount_quote_lots` from the depositor's ATA
    /// to the vault's collateral PDA ships in phase 9b via inverse
    /// CPI from core. Until then the deposit is "pledged" — shares
    /// are minted on the assumption that the SPL transfer follows
    /// in the same tx (caller composes both ixs).
    pub fn vault_deposit_v3(
        ctx: Context<VaultDepositV3>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, VaultsError::ZeroSize);
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
    /// Same SPL-transfer caveat as `vault_deposit_v3` — phase 9b
    /// wires the actual token movement.
    pub fn vault_withdraw_v3(
        ctx: Context<VaultWithdrawV3>,
        shares_to_burn: u64,
    ) -> Result<()> {
        require!(shares_to_burn > 0, VaultsError::ZeroSize);
        let vault = &mut ctx.accounts.vault;
        let pos = &mut ctx.accounts.position;

        require!(pos.shares >= shares_to_burn, VaultsError::OutOfRange);
        require!(
            vault.shares_outstanding >= shares_to_burn,
            VaultsError::OutOfRange
        );

        // Pro-rata withdrawal:
        //   amount = shares × total_capital / shares_outstanding
        let amount_u128 = (shares_to_burn as u128)
            .checked_mul(vault.total_capital_quote_lots as u128)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?
            / (vault.shares_outstanding as u128);
        let amount: u64 = if amount_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            amount_u128 as u64
        };

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

        emit!(VaultWithdrawV3Event {
            vault: vault.key(),
            depositor: pos.depositor,
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            shares_outstanding_after: vault.shares_outstanding,
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
