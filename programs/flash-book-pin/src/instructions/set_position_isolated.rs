//! set_position_isolated — convert a CROSS position to ISOLATED: move
//! `amount_quote_lots` from the trader's cross pool into the position's own
//! bucket, and assess that BOTH (a) the now-isolated target is healthy on
//! `amount` alone, and (b) the REMAINING cross positions are healthy on the
//! reduced `cross − amount` pool. Self-action (the signing trader). Faithful
//! port of the Anchor `set_position_isolated`.
//!
//! Pin's `assess_margin` takes a single collateral, so the anchor
//! `assess_margin_split` (target-vs-bucket + cross-vs-pool) is emulated with TWO
//! checks: the target alone against `amount`, and the siblings against
//! `cross − amount`. H-1 coverage: exactly `(open − 1)` sibling pairs, each CROSS
//! + distinct market.
//!
//! accounts: [trader (signer), trader_state (owned, w), target_market,
//!            target_position (owned, w), <market, position> × (open−1)]
//! data: amount_quote_lots (u64 LE)

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

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, target_market, target_position, siblings @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }

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
    if tp_collat != 0 {
        return Err(ProgramError::InvalidArgument); // already isolated
    }
    if amount > ts_collat {
        return Err(ProgramError::InsufficientFunds);
    }
    let post_cross = ts_collat - amount;

    // H-1 coverage: exactly (open − 1) sibling pairs.
    if siblings.len() % 2 != 0 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let sib_count = siblings.len() / 2;
    if sib_count != (ts_open.saturating_sub(1)) as usize {
        return Err(ProgramError::InvalidArgument);
    }
    if sib_count > MAX_PORTFOLIO {
        return Err(ProgramError::InvalidInstructionData);
    }
    let no_shock: &[StressShock] = &[];

    // ── (a) the now-isolated target must be healthy on `amount` alone ───
    let Some((t_pos, t_mkt, _c)) =
        build_snapshot(pid, target_market, trader_state, target_position, &[])?
    else {
        return Err(ProgramError::InvalidArgument);
    };
    let target_ok = assess_margin(&[t_pos], &[t_mkt], &[no_shock], amount)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !target_ok.is_healthy {
        return Err(ProgramError::InsufficientFunds);
    }

    // ── (b) the remaining CROSS positions must be healthy on `post_cross` ─
    let mut positions = [zero_position(); MAX_PORTFOLIO];
    let mut markets = [zero_market(); MAX_PORTFOLIO];
    let mut seen: [PubkeyBytes; MAX_PORTFOLIO] = [[0u8; 32]; MAX_PORTFOLIO];
    let mut n = 0usize;
    for i in 0..sib_count {
        let m_ai = &siblings[2 * i];
        let p_ai = &siblings[2 * i + 1];
        let Some((pos_snap, mkt_snap, _c)) = build_snapshot(pid, m_ai, trader_state, p_ai, &[])?
        else {
            return Err(ProgramError::InvalidArgument);
        };
        let sib_collat = {
            let p = unsafe { &*(p_ai.borrow_data_unchecked().as_ptr() as *const Position) };
            p.collateral_quote_lots
        };
        if sib_collat != 0 {
            return Err(ProgramError::InvalidArgument); // siblings must stay cross
        }
        if m_ai.key() == target_market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if seen[..n].iter().any(|k| k == m_ai.key()) {
            return Err(ProgramError::InvalidArgument);
        }
        seen[n] = *m_ai.key();
        positions[n] = pos_snap;
        markets[n] = mkt_snap;
        n += 1;
    }
    if n > 0 {
        let cross_ok = assess_margin(&positions[..n], &markets[..n], &[no_shock], post_cross)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        if !cross_ok.is_healthy {
            return Err(ProgramError::InsufficientFunds);
        }
    }

    // ── apply: cross pool → the target's isolated bucket ────────────────
    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = post_cross;
        let p = &mut *(target_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut Position);
        p.collateral_quote_lots = amount;
    }
    Ok(())
}
