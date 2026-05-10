#![allow(unexpected_cfgs)]
//! Flash Book FLP — wave 21 program split.
//!
//! Owns `FlpExposurePerMarketAccountV3` (PDA at `[b"flp_per_market", market]`,
//! one per market). Solves the singleton bottleneck that prevents
//! ER-delegating FLP per-market in the legacy `flash-book` program —
//! each market's FLP exposure is now an independent account that can
//! be delegated to a different ER instance.
//!
//! ── Status: PHASE 8 — account type + init ix shipped
//!
//! Functional ixs that match the legacy core surface:
//!   • `init_flp_per_market_v3`  — create a per-market FLP exposure
//!   • `record_flp_fill_v3`      — wrapper-side bookkeeping after a
//!                                 fill (called by core via inverse-CPI
//!                                 from `apply_flp_fill` once that ix
//!                                 gets a v3 variant in a follow-up wave)
//!
//! NOT yet shipped (deferred):
//!   • SPL deposit/withdraw paths — these need to stay routed through
//!     core's `InsuranceFundAccount` PDA which owns the quote vault.
//!     Wrapper-side `deposit_flp_capital_v3` would CPI into a new core
//!     ix that does the SPL transfer + credits the wrapper's
//!     per-market account. That's a focused follow-up because the
//!     auth model needs care (wrapper signs over a different PDA than
//!     the trader; core verifies the calling program is in the wave-21
//!     whitelist; SPL transfer is from trader's ATA to insurance vault).

use anchor_lang::prelude::*;

declare_id!("eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M");

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

#[error_code]
pub enum FlpError {
    #[msg("argument out of allowed range")]
    OutOfRange,
    #[msg("size cannot be zero")]
    ZeroSize,
    #[msg("price cannot be zero")]
    ZeroPrice,
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
