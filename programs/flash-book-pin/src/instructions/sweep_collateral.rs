//! sweep_collateral — move quote collateral between two TraderStates the signer
//! is authorized for, gated by a POST-sweep portfolio health check on the source.
//! Faithful port of the Anchor `sweep_collateral`. No funds leave the protocol
//! (state-to-state), so this is NON-CPI and fully e2e-testable.
//!
//! If the source has open positions, the caller passes its full portfolio as
//! `[market, position]` pairs in the trailing accounts (count == open_positions);
//! each position is bound to the source via `build_snapshot`, markets must be
//! distinct, and the post-sweep collateral must survive a PROTOCOL-ENFORCED
//! correlated stress battery (not caller-supplied — else a weak battery could
//! bypass the gate). Reject if any shock breaches maintenance.
//!
//! accounts: [authority (signer), from_state (w), to_state (w),
//!            (market, position) * from.open_positions]
//! data: amount (u64 LE)

use crate::instructions::margin_probe::build_snapshot;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::state::{TraderState, TRADER_STATE_DISC};
use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

const MAX_PORTFOLIO: usize = 8;

/// Protocol-enforced correlated stress battery (bps applied to EVERY portfolio
/// market at once). Mirrors the magnitude lattice of anchor's `default_scenarios`
/// (single-market shocks ± the correlated all-down/all-up legs), reduced to the
/// correlated rows for SBF stack tractability — the dominant risk for a cross
/// portfolio. Withdraw-style gate ⇒ assessed against INITIAL margin upstream.
const STRESS_BPS: [i32; 10] = [-3000, -2000, -1000, -500, -200, 200, 500, 1000, 2000, 3000];

fn zero_position() -> PositionSnapshot {
    PositionSnapshot {
        market: [0u8; 32], side: Side::Long, size_lots: 0, entry_price: Ticks(0),
        cum_funding_index_at_entry: 0, collateral_quote_lots: 0,
    }
}
fn zero_market() -> MarketSnapshot {
    MarketSnapshot {
        market: [0u8; 32], mark_price: Ticks(0), cum_funding_index: 0, maintenance_margin_bps: 0,
        tick_size: 0, concentration_threshold_lots: 0, concentration_extra_mmr_bps: 0,
        side_oi_lots: 0, oi_mmr_slope_bps_per_million_lots: 0, oi_mmr_max_extra_bps: 0,
    }
}

fn is_authorized(ts: &TraderState, signer: &Pubkey) -> bool {
    &ts.trader == signer || &ts.delegate == signer
}

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, from_state, to_state, pairs @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(authority)?;
    assert_owned_by(from_state, pid)?;
    assert_disc(from_state, &TRADER_STATE_DISC)?;
    assert_owned_by(to_state, pid)?;
    assert_disc(to_state, &TRADER_STATE_DISC)?;
    if from_state.key() == to_state.key() {
        return Err(ProgramError::InvalidArgument);
    }

    let (from_trader, from_open, from_collat) = {
        let d = from_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        if !is_authorized(s, authority.key()) {
            return Err(ProgramError::IllegalOwner);
        }
        // ER-active sources are fail-closed: their attested `reserved_margin`
        // backs resting orders held in the ER, which do NOT appear in the
        // `open_positions` portfolio walk below — so the stress gate cannot see
        // (and the `from_open == 0` branch entirely skips) that reservation. A
        // sweep would relocate the backing collateral with zero ER awareness,
        // exactly the move the strict withdraw gate forbids. Force the ER trader
        // to settle/withdraw via the xdomain path first.
        if s.er_active != 0 {
            return Err(ProgramError::Custom(241)); // resolve ER reservation first
        }
        (s.trader, s.open_positions, s.collateral_quote_lots)
    };
    {
        let d = to_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        if !is_authorized(s, authority.key()) {
            return Err(ProgramError::IllegalOwner);
        }
        if s.trader == from_trader {
            return Err(ProgramError::InvalidArgument); // distinct traders (anchor parity)
        }
    }

    let post_sweep_collat = from_collat
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;

    // ── post-sweep portfolio health gate (only if the source has positions) ─
    if from_open > 0 {
        let expected = from_open as usize;
        if expected > MAX_PORTFOLIO || pairs.len() != expected * 2 {
            return Err(ProgramError::InvalidArgument);
        }
        let mut positions = [zero_position(); MAX_PORTFOLIO];
        let mut markets = [zero_market(); MAX_PORTFOLIO];
        let mut seen: [[u8; 32]; MAX_PORTFOLIO] = [[0u8; 32]; MAX_PORTFOLIO];
        for i in 0..expected {
            let m_ai = &pairs[2 * i];
            let p_ai = &pairs[2 * i + 1];
            let Some((pos, mut mkt, _c)) = build_snapshot(pid, m_ai, from_state, p_ai, &[])? else {
                return Err(ProgramError::InvalidArgument);
            };
            if seen[..i].iter().any(|k| k == m_ai.key()) {
                return Err(ProgramError::InvalidArgument); // duplicate market
            }
            // Withdraw-style gate ⇒ assess against INITIAL margin, not maintenance
            // (parity with partial_withdraw). `build_snapshot` fills the MAINTENANCE
            // MMR; override it so a sweep can't strip the IM buffer down to the
            // liquidation line and then exit the (now-flat) sibling via withdraw.
            let max_lev = unsafe {
                (*(m_ai.borrow_data_unchecked().as_ptr() as *const crate::state::Market)).max_leverage
            };
            mkt.maintenance_margin_bps =
                crate::instructions::partial_withdraw::im_bps(mkt.maintenance_margin_bps, max_lev);
            seen[i] = *m_ai.key();
            positions[i] = pos;
            markets[i] = mkt;
        }
        // Evaluate each correlated shock level as its own scenario; reject on any breach.
        let mut row = [StressShock { market: [0u8; 32], shock_bps: 0 }; MAX_PORTFOLIO];
        for &bps in STRESS_BPS.iter() {
            for (cell, m) in row[..expected].iter_mut().zip(markets[..expected].iter()) {
                *cell = StressShock { market: m.market, shock_bps: bps };
            }
            let scenario: &[StressShock] = &row[..expected];
            let assessment = assess_margin(&positions[..expected], &markets[..expected], &[scenario], post_sweep_collat)
                .map_err(|_| ProgramError::ArithmeticOverflow)?;
            if !assessment.is_healthy {
                return Err(ProgramError::Custom(230)); // would be liquidatable post-sweep
            }
        }
    }

    // ── move the collateral (checks-effects) ────────────────────────────
    unsafe {
        let f = &mut *(from_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        f.collateral_quote_lots = post_sweep_collat;
        let t = &mut *(to_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        t.collateral_quote_lots = t
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
