//! set_position_cross — convert an ISOLATED position back to CROSS margin: merge
//! its per-position bucket into the trader's cross pool and assess that the WHOLE
//! cross portfolio (every sibling + the now-cross target) is still healthy at the
//! combined collateral. Self-action (the signing trader). Faithful port of the
//! Anchor `set_position_cross`.
//!
//! H-1 coverage: the target is named, so EVERY other open position must be
//! supplied as a `(market, position)` pair — exactly `(open_positions − 1)` pairs
//! — and each sibling must be CROSS (bucket 0) and a distinct market. Without
//! full coverage the joint margin check could omit a leg and wave through an
//! unhealthy move.
//!
//! Health gate: zero-shock base-maintenance `assess_margin` against the combined
//! `cross + returned` collateral (the `verify_solvency` stance; `assess_margin`
//! uses ONLY the passed collateral, so the target's snapshot is assessed against
//! the pooled collateral exactly like a cross leg). Blast radius is the signer's
//! own portfolio.
//!
//! accounts: [trader (signer), trader_state (owned, w), target_market,
//!            target_position (owned, w), <market, position> × (open−1)]
//! data: (none)

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::instructions::margin_probe::build_snapshot;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::state::{Position, Pubkey as PubkeyBytes, TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Siblings + the target. A trader with more open positions splits the call.
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
    let [trader, trader_state, target_market, target_position, siblings @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(trader)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_market(target_market, pid)?;
    assert_position(target_position, pid)?;

    let (ts_trader, ts_collat, ts_open) = {
        let ts = unsafe { &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState) };
        (ts.trader, ts.collateral_quote_lots, ts.open_positions)
    };
    let (tp_trader, tp_market, tp_collat, tp_size) = {
        let p = unsafe { &*(target_position.borrow_data_unchecked().as_ptr() as *const Position) };
        (p.trader, p.market, p.collateral_quote_lots, p.size_lots)
    };

    if ts_trader != *trader.key() || tp_trader != ts_trader {
        return Err(ProgramError::InvalidArgument);
    }
    if tp_market != *target_market.key() || tp_size == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if tp_collat == 0 {
        return Err(ProgramError::InvalidArgument); // already cross
    }
    let returned = tp_collat;

    if siblings.len() % 2 != 0 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let sib_count = siblings.len() / 2;
    if sib_count != (ts_open.saturating_sub(1)) as usize {
        return Err(ProgramError::InvalidArgument); // H-1 coverage
    }
    if sib_count + 1 > MAX_PORTFOLIO {
        return Err(ProgramError::InvalidInstructionData);
    }

    let mut positions = [zero_position(); MAX_PORTFOLIO];
    let mut markets = [zero_market(); MAX_PORTFOLIO];
    let mut seen: [PubkeyBytes; MAX_PORTFOLIO] = [[0u8; 32]; MAX_PORTFOLIO];
    let mut n = 0usize;

    for i in 0..sib_count {
        let m_ai = &siblings[2 * i];
        let p_ai = &siblings[2 * i + 1];
        let Some((pos_snap, mkt_snap, _c)) = build_snapshot(pid, m_ai, trader_state, p_ai, &[])?
        else {
            return Err(ProgramError::InvalidArgument); // sibling flat
        };
        let sib_collat = {
            let p = unsafe { &*(p_ai.borrow_data_unchecked().as_ptr() as *const Position) };
            p.collateral_quote_lots
        };
        if sib_collat != 0 {
            return Err(ProgramError::InvalidArgument); // siblings must be cross
        }
        if m_ai.key() == target_market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if seen[..n].iter().any(|k| k == m_ai.key()) {
            return Err(ProgramError::InvalidArgument); // duplicate market
        }
        seen[n] = *m_ai.key();
        positions[n] = pos_snap;
        markets[n] = mkt_snap;
        n += 1;
    }

    // Add the target, assessed post-transition as a cross leg.
    let Some((pos_snap, mkt_snap, _c)) =
        build_snapshot(pid, target_market, trader_state, target_position, &[])?
    else {
        return Err(ProgramError::InvalidArgument);
    };
    positions[n] = pos_snap;
    markets[n] = mkt_snap;
    n += 1;

    let post_cross_collateral = ts_collat
        .checked_add(returned)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let no_shock: &[StressShock] = &[];
    let assessment =
        assess_margin(&positions[..n], &markets[..n], &[no_shock], post_cross_collateral)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        return Err(ProgramError::InsufficientFunds);
    }

    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = post_cross_collateral;
        let p = &mut *(target_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut Position);
        p.collateral_quote_lots = 0;
    }
    Ok(())
}
