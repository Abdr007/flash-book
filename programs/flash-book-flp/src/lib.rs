#![allow(unexpected_cfgs)]
//! Flash Book FLP — wave 21 program split.
//!
//! Owns `FlpExposurePerMarketAccountV3` (PDA at `[b"flp_per_market", market]`,
//! one per market). Solves the singleton bottleneck that prevents
//! ER-delegating FLP per-market in the legacy `flash-book` program —
//! each market's FLP exposure is now an independent account that can
//! be delegated to a different ER instance.
//!
//! ── Status: PHASE 8 + 8b — full SPL deposit/withdraw shipped
//!
//! Ixs:
//!   • `init_flp_per_market_v3`     — create a per-market FLP exposure
//!   • `record_flp_fill_v3`         — wrapper-side fill bookkeeping
//!   • `flp_deposit_v3`             — depositor signs SPL transfer
//!                                    (their ATA → core's quote_vault)
//!                                    + wrapper mints LP shares pro-rata
//!   • `flp_withdraw_v3`            — wrapper burns shares + CPIs into
//!                                    core's `cpi_release_collateral_to_user`
//!                                    to pay the LP from the protocol
//!                                    quote_vault (signed by core's
//!                                    InsuranceFund PDA).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use flash_book::cpi::accounts::CpiReleaseCollateralToUser as CoreCpiReleaseCollateralToUser;
use flash_book::program::FlashBook;
use flash_book::state::{FlpExposureAccount as CoreFlpExposureAccount, InsuranceFundAccount};

declare_id!("eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M");

/// Seed for this program's CPI authority PDA — must match the value
/// hardcoded in core's `WAVE21_FLP_PROGRAM_ID` whitelist (core derives
/// `find_program_address(&[CPI_AUTHORITY_SEED], &flp_program_id)` and
/// expects the signer to match).
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

#[program]
pub mod flash_book_flp {
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

    /// Initialize a per-market FLP exposure account. Authority-only.
    /// One per market — different markets get independent accounts so
    /// they can ER-delegate independently (which the singleton in core
    /// can't do without bottlenecking all markets to one ER instance).
    pub fn init_flp_per_market_v3(ctx: Context<InitFlpPerMarketV3>) -> Result<()> {
        let acct = &mut ctx.accounts.exposure;
        acct.market = ctx.accounts.market.key();
        acct.authority = ctx.accounts.authority.key();
        acct.bump = ctx.bumps.exposure;
        acct.side = 255; // empty
        acct.size_lots = 0;
        acct.entry_price_ticks = 0;
        acct.total_capital_quote_lots = 0;
        acct.realized_pnl = 0;
        acct.lp_shares_outstanding = 0;

        emit!(FlpPerMarketInitV3Event {
            market: acct.market,
            authority: acct.authority,
        });
        Ok(())
    }

    /// Record a fill on this per-market FLP exposure. Authority-gated
    /// (only this program's CPI authority OR the configured authority
    /// can call). In the modular flow, core's matcher tick CPIs into
    /// this ix after each FLP-maker fill. For phase 8 MVP it's
    /// authority-callable so off-chain settlement can update state
    /// without waiting for the core CPI handshake to ship.
    pub fn record_flp_fill_v3(
        ctx: Context<RecordFlpFillV3>,
        size_lots: u64,
        price_ticks: u64,
        side: u8,
        realized_pnl_delta: i64,
    ) -> Result<()> {
        require!(side <= 1, FlpError::OutOfRange);
        require!(size_lots > 0, FlpError::ZeroSize);
        require!(price_ticks > 0, FlpError::ZeroPrice);

        let acct = &mut ctx.accounts.exposure;
        require!(
            acct.authority == ctx.accounts.authority.key(),
            FlpError::Unauthorized
        );

        // Position bookkeeping — same logic core uses for the singleton's
        // per_market[i] entry: same-side adds; opposite-side reduces or
        // flips. (Volume-weighted average entry for adds.)
        if acct.size_lots == 0 {
            acct.side = side;
            acct.size_lots = size_lots;
            acct.entry_price_ticks = price_ticks;
        } else if acct.side == side {
            // Same side: weighted-average entry.
            let new_size = acct.size_lots.saturating_add(size_lots);
            let new_entry_u128 = (acct.entry_price_ticks as u128)
                .saturating_mul(acct.size_lots as u128)
                .saturating_add((price_ticks as u128).saturating_mul(size_lots as u128))
                / new_size as u128;
            acct.size_lots = new_size;
            acct.entry_price_ticks =
                if new_entry_u128 > u64::MAX as u128 { u64::MAX } else { new_entry_u128 as u64 };
        } else if size_lots <= acct.size_lots {
            acct.size_lots -= size_lots;
            if acct.size_lots == 0 {
                acct.side = 255;
                acct.entry_price_ticks = 0;
            }
        } else {
            // Flip.
            let remaining = size_lots - acct.size_lots;
            acct.side = side;
            acct.size_lots = remaining;
            acct.entry_price_ticks = price_ticks;
        }

        acct.realized_pnl = acct.realized_pnl.saturating_add(realized_pnl_delta);

        emit!(FlpFillRecordedV3Event {
            market: acct.market,
            side,
            size_lots,
            price_ticks,
            realized_pnl_delta,
            new_side: acct.side,
            new_size_lots: acct.size_lots,
            new_realized_pnl: acct.realized_pnl,
        });
        Ok(())
    }

    /// Wave 21 phase 8b — deposit FLP capital with REAL SPL transfer.
    ///
    /// LP signs the SPL transfer from their ATA → core's `quote_vault`
    /// directly (LP owns their ATA, no PDA signing required for the IN
    /// direction). Wrapper records the deposit + mints LP shares pro-
    /// rata to current NAV.
    ///
    /// Bootstrap: when `lp_shares_outstanding == 0`, mint 1 share per
    /// quote-lot deposited. Otherwise: shares = amount × shares_outstanding
    /// / total_capital. Same math as core's `deposit_flp_capital`.
    pub fn flp_deposit_v3(ctx: Context<FlpDepositV3>, amount_quote_lots: u64) -> Result<()> {
        require!(amount_quote_lots > 0, FlpError::ZeroSize);

        let acct = &mut ctx.accounts.exposure;

        // Pull tokens from LP → quote_vault. LP signs as ATA owner.
        let cpi_accounts = Transfer {
            from: ctx.accounts.lp_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.lp.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        // Compute share mint.
        let shares = if acct.lp_shares_outstanding == 0 || acct.total_capital_quote_lots == 0 {
            amount_quote_lots
        } else {
            let s = (amount_quote_lots as u128)
                .saturating_mul(acct.lp_shares_outstanding as u128)
                / (acct.total_capital_quote_lots as u128).max(1);
            if s > u64::MAX as u128 { u64::MAX } else { s as u64 }
        };
        require!(shares > 0, FlpError::ZeroShares);

        // Update state on credit.
        acct.total_capital_quote_lots = acct
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or(FlpError::Overflow)?;
        acct.lp_shares_outstanding = acct
            .lp_shares_outstanding
            .checked_add(shares)
            .ok_or(FlpError::Overflow)?;

        // Position-level shares — depositor's running balance.
        let pos = &mut ctx.accounts.position;
        if pos.shares == 0 {
            pos.market = acct.market;
            pos.lp = ctx.accounts.lp.key();
            pos.bump = ctx.bumps.position;
        }
        pos.shares = pos
            .shares
            .checked_add(shares)
            .ok_or(FlpError::Overflow)?;

        emit!(FlpDepositedV3Event {
            market: acct.market,
            lp: ctx.accounts.lp.key(),
            amount_quote_lots,
            shares_minted: shares,
            new_total_capital: acct.total_capital_quote_lots,
            new_shares_outstanding: acct.lp_shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 8b — withdraw FLP capital by burning shares.
    ///
    /// Wrapper computes the pro-rata payout, burns the shares, then
    /// CPIs into core's `cpi_release_collateral_to_user` which signs
    /// the SPL transfer from `quote_vault` → LP's ATA as the
    /// InsuranceFund PDA. This is the core inverse-CPI authority gate
    /// added in the same wave (whitelisted to the 3 wrapper programs).
    pub fn flp_withdraw_v3(ctx: Context<FlpWithdrawV3>, shares_to_burn: u64) -> Result<()> {
        require!(shares_to_burn > 0, FlpError::ZeroShares);

        let market_key = ctx.accounts.exposure.market;
        let pos_shares = ctx.accounts.position.shares;
        require!(shares_to_burn <= pos_shares, FlpError::InsufficientShares);

        let total_capital = ctx.accounts.exposure.total_capital_quote_lots;
        let total_shares = ctx.accounts.exposure.lp_shares_outstanding;
        require!(total_shares > 0, FlpError::NotInitialized);

        // Pro-rata payout: shares × total_capital / total_shares.
        let amount_u128 = (shares_to_burn as u128).saturating_mul(total_capital as u128)
            / (total_shares as u128);
        let amount = if amount_u128 > u64::MAX as u128 { u64::MAX } else { amount_u128 as u64 };
        require!(amount > 0, FlpError::ZeroSize);

        // Burn shares + decrement capital BEFORE the CPI (defensive).
        {
            let acct = &mut ctx.accounts.exposure;
            acct.lp_shares_outstanding = acct
                .lp_shares_outstanding
                .checked_sub(shares_to_burn)
                .ok_or(FlpError::Underflow)?;
            acct.total_capital_quote_lots = acct
                .total_capital_quote_lots
                .checked_sub(amount)
                .ok_or(FlpError::Underflow)?;
            let pos = &mut ctx.accounts.position;
            pos.shares = pos
                .shares
                .checked_sub(shares_to_burn)
                .ok_or(FlpError::Underflow)?;
        }

        // CPI into core for SPL release. Wrapper signs as its CPI authority PDA.
        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CoreCpiReleaseCollateralToUser {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            insurance_fund: ctx.accounts.insurance_fund.to_account_info(),
            quote_vault: ctx.accounts.quote_vault.to_account_info(),
            user_quote_ata: ctx.accounts.lp_quote_ata.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cpi_release_collateral_to_user(cpi_ctx, amount)?;

        emit!(FlpWithdrawnV3Event {
            market: market_key,
            lp: ctx.accounts.lp.key(),
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            new_total_capital: ctx.accounts.exposure.total_capital_quote_lots,
            new_shares_outstanding: ctx.accounts.exposure.lp_shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 10 — migrate one row of the legacy core FLP
    /// singleton's `per_market[]` array into a per-market v3 account.
    /// Authority-gated (legacy authority must match the v3 authority).
    ///
    /// Capital + shares allocation across markets is a governance
    /// decision (the legacy singleton pools NAV across all markets;
    /// v3 splits NAV per-market). This ix takes explicit
    /// `total_capital_quote_lots` + `lp_shares_outstanding` allocated
    /// to this market by the authority — typically pro-rated by
    /// notional exposure at migration time.
    pub fn migrate_flp_market_to_v3(
        ctx: Context<MigrateFlpMarketToV3>,
        market_index: u8,
        allocated_total_capital_quote_lots: u64,
        allocated_lp_shares_outstanding: u64,
        allocated_realized_pnl: i64,
    ) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            (market_index as usize) < src.per_market.len(),
            FlpError::OutOfRange
        );
        require!(
            src.authority == ctx.accounts.authority.key(),
            FlpError::Unauthorized
        );

        let row = src.per_market[market_index as usize];
        require!(row.market == ctx.accounts.market.key(), FlpError::OutOfRange);

        // Sanity: allocations cannot exceed singleton totals.
        require!(
            allocated_total_capital_quote_lots <= src.total_capital_quote_lots,
            FlpError::OutOfRange
        );
        require!(
            allocated_lp_shares_outstanding <= src.lp_shares_outstanding,
            FlpError::OutOfRange
        );

        let dst = &mut ctx.accounts.v3;
        dst.market = row.market;
        dst.authority = src.authority;
        dst.bump = ctx.bumps.v3;
        dst.side = row.side;
        dst._pad0 = [0; 6];
        dst.size_lots = row.size_lots;
        dst.entry_price_ticks = row.entry_price_ticks;
        dst.total_capital_quote_lots = allocated_total_capital_quote_lots;
        dst.realized_pnl = allocated_realized_pnl;
        dst.lp_shares_outstanding = allocated_lp_shares_outstanding;

        emit!(LegacyFlpMarketMigratedV3Event {
            market: row.market,
            market_index,
            legacy_singleton: src.key(),
            v3: dst.key(),
            allocated_total_capital_quote_lots,
            allocated_lp_shares_outstanding,
        });
        Ok(())
    }
}

/// Per-market FLP exposure. Replaces the singleton FlpExposureAccount
/// in core for this market. ER-delegatable per-market.
#[account]
#[derive(Debug)]
pub struct FlpExposurePerMarketAccountV3 {
    pub market: Pubkey,
    pub authority: Pubkey,
    pub bump: u8,
    /// 0 = long, 1 = short, 255 = empty (no position).
    pub side: u8,
    pub _pad0: [u8; 6],
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    /// LP-share/NAV bookkeeping mirrored from core (will be split into
    /// a dedicated LpSharesAccount in phase 8b once the modular shape
    /// is final).
    pub total_capital_quote_lots: u64,
    pub realized_pnl: i64,
    pub lp_shares_outstanding: u64,
}

impl FlpExposurePerMarketAccountV3 {
    pub const SEED: &'static [u8] = b"flp_per_market";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 1 + 6 + 8 + 8 + 8 + 8 + 8 = 120. Round 128.
        8 + 128
    }
}

/// Per-LP, per-market shares balance. Tracks how many shares of a
/// given market's FLP this LP holds. Created on first deposit.
#[account]
#[derive(Debug)]
pub struct FlpPositionAccountV3 {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub bump: u8,
    pub _pad: [u8; 7],
    pub shares: u64,
}

impl FlpPositionAccountV3 {
    pub const SEED: &'static [u8] = b"flp_position_v3";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 7 + 8 = 88. Round 96.
        8 + 96
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct InitFlpPerMarketV3<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: market pubkey — used as the PDA seed only.
    pub market: UncheckedAccount<'info>,

    #[account(
        init,
        payer = authority,
        space = FlpExposurePerMarketAccountV3::space(),
        seeds = [FlpExposurePerMarketAccountV3::SEED, market.key().as_ref()],
        bump,
    )]
    pub exposure: Account<'info, FlpExposurePerMarketAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RecordFlpFillV3<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, FlpExposurePerMarketAccountV3>,
}

#[derive(Accounts)]
pub struct FlpDepositV3<'info> {
    #[account(mut)]
    pub lp: Signer<'info>,

    #[account(
        mut,
        seeds = [FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, FlpExposurePerMarketAccountV3>,

    /// Per-LP shares balance — `init_if_needed` since the first deposit
    /// for an (lp, market) pair creates it.
    #[account(
        init_if_needed,
        payer = lp,
        space = FlpPositionAccountV3::space(),
        seeds = [FlpPositionAccountV3::SEED, exposure.key().as_ref(), lp.key().as_ref()],
        bump,
    )]
    pub position: Account<'info, FlpPositionAccountV3>,

    /// LP's USDC ATA — debited.
    #[account(mut)]
    pub lp_quote_ata: Account<'info, TokenAccount>,

    /// Core's protocol vault — credited.
    #[account(mut)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FlpWithdrawV3<'info> {
    #[account(mut)]
    pub lp: Signer<'info>,

    #[account(
        mut,
        seeds = [FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, FlpExposurePerMarketAccountV3>,

    #[account(
        mut,
        seeds = [FlpPositionAccountV3::SEED, exposure.key().as_ref(), lp.key().as_ref()],
        bump = position.bump,
        constraint = position.lp == lp.key() @ FlpError::Unauthorized,
    )]
    pub position: Account<'info, FlpPositionAccountV3>,

    /// CHECK: this program's CPI authority — must derive from
    /// `[CPI_AUTHORITY_SEED]` under this program ID. Anchor `seeds`
    /// constraint enforces it.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// Core's InsuranceFund PDA — signs the SPL transfer out.
    /// Anchor verifies via the `seeds` constraint that this is the
    /// canonical InsuranceFund of `flash_book_program`.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
        seeds::program = flash_book_program.key(),
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Core's protocol vault — debited via core CPI.
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// LP's USDC ATA — credited via core CPI.
    #[account(mut)]
    pub lp_quote_ata: Account<'info, TokenAccount>,

    pub flash_book_program: Program<'info, FlashBook>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct MigrateFlpMarketToV3<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// Legacy core FLP singleton — read-only.
    pub legacy: Account<'info, CoreFlpExposureAccount>,

    /// CHECK: market pubkey — used as the v3 PDA seed only and matched
    /// against the legacy `per_market[market_index].market` row.
    pub market: UncheckedAccount<'info>,

    /// Destination v3 per-market account.
    #[account(
        init,
        payer = authority,
        space = FlpExposurePerMarketAccountV3::space(),
        seeds = [FlpExposurePerMarketAccountV3::SEED, market.key().as_ref()],
        bump,
    )]
    pub v3: Account<'info, FlpExposurePerMarketAccountV3>,

    pub system_program: Program<'info, System>,
}

#[error_code]
pub enum FlpError {
    #[msg("argument out of allowed range")]
    OutOfRange,
    #[msg("size cannot be zero")]
    ZeroSize,
    #[msg("price cannot be zero")]
    ZeroPrice,
    #[msg("share quantity cannot be zero")]
    ZeroShares,
    #[msg("insufficient shares for withdrawal")]
    InsufficientShares,
    #[msg("FLP exposure not initialized")]
    NotInitialized,
    #[msg("arithmetic overflow")]
    Overflow,
    #[msg("arithmetic underflow")]
    Underflow,
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
pub struct FlpPerMarketInitV3Event {
    pub market: Pubkey,
    pub authority: Pubkey,
}

#[event]
pub struct FlpFillRecordedV3Event {
    pub market: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub realized_pnl_delta: i64,
    pub new_side: u8,
    pub new_size_lots: u64,
    pub new_realized_pnl: i64,
}

#[event]
pub struct FlpDepositedV3Event {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub amount_quote_lots: u64,
    pub shares_minted: u64,
    pub new_total_capital: u64,
    pub new_shares_outstanding: u64,
}

#[event]
pub struct FlpWithdrawnV3Event {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub shares_burned: u64,
    pub amount_quote_lots: u64,
    pub new_total_capital: u64,
    pub new_shares_outstanding: u64,
}

#[event]
pub struct LegacyFlpMarketMigratedV3Event {
    pub market: Pubkey,
    pub market_index: u8,
    pub legacy_singleton: Pubkey,
    pub v3: Pubkey,
    pub allocated_total_capital_quote_lots: u64,
    pub allocated_lp_shares_outstanding: u64,
}
