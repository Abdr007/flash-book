//! verify_portfolio_solvency — READ-ONLY cross-margin check across ALL of a
//! trader's positions at once. `verify_solvency` probes one position; this runs
//! the proven `risk::assess_margin` over the whole supplied set against the
//! trader's single shared collateral pool, so offsetting positions net correctly
//! and the maintenance requirement is the joint one. Reverts `Custom(101)` if the
//! portfolio is under maintenance. Mutates NO state.
//!
//! Each position is bound (inside the shared `build_snapshot`) to
//! `trader_state.trader` and its paired market, so a caller cannot dilute the
//! check with another trader's position; duplicating a position only makes the
//! requirement stricter (revert-only), never weaker. A keeper passes the trader's
//! full position set to get the true portfolio health.
//!
//! accounts: [trader_state, <market, position> × N]  (N up to MAX_PORTFOLIO)
//! data: none

use crate::instructions::margin_probe::build_snapshot;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Cap on positions assessed in one call (bounds CU + the stack snapshot arrays).
const MAX_PORTFOLIO: usize = 8;

#[inline]
fn zero_position() -> PositionSnapshot {
    PositionSnapshot {
        market: [0u8; 32],
        side: Side::Long,
        size_lots: 0,
        entry_price: Ticks(0),
        cum_funding_index_at_entry: 0,
        collateral_quote_lots: 0,
    }
}

#[inline]
fn zero_market() -> MarketSnapshot {
    MarketSnapshot {
        market: [0u8; 32],
        mark_price: Ticks(0),
        cum_funding_index: 0,
        maintenance_margin_bps: 0,
        tick_size: 0,
        concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0,
        side_oi_lots: 0,
        oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
    }
}

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader_state, pairs @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    // Remaining accounts come as (market, position) pairs — must be even.
    if pairs.len() % 2 != 0 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let pair_count = pairs.len() / 2;
    if pair_count > MAX_PORTFOLIO {
        return Err(ProgramError::InvalidInstructionData);
    }

    let mut positions = [zero_position(); MAX_PORTFOLIO];
    let mut markets = [zero_market(); MAX_PORTFOLIO];
    let mut collateral: u64 = 0;
    let mut n = 0usize;

    for i in 0..pair_count {
        let market = &pairs[2 * i];
        let position = &pairs[2 * i + 1];
        // `build_snapshot` runs every guard + the trader/market binding and
        // returns None for a flat position (skip). No tiers account here ⇒ flat
        // base MMR (per-market tiers in the portfolio path are a follow-up).
        if let Some((pos_snap, mkt_snap, c)) =
            build_snapshot(pid, market, trader_state, position, &[])?
        {
            positions[n] = pos_snap;
            markets[n] = mkt_snap;
            collateral = c; // identical across pairs (same trader_state)
            n += 1;
        }
    }

    // All positions flat (or none supplied) ⇒ trivially solvent.
    if n == 0 {
        return Ok(());
    }

    // Single ZERO-shock scenario so the joint MAINTENANCE requirement is priced
    // at mark (see verify_solvency for why an empty set would only check ≥ 0).
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&positions[..n], &markets[..n], &[no_shock], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        return Err(ProgramError::Custom(101));
    }
    Ok(())
}
