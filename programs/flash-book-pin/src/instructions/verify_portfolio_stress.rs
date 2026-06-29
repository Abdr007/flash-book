//! verify_portfolio_stress — READ-ONLY portfolio stress lattice. Combines both
//! risk axes: a trader's WHOLE book (N positions, cross-margined against one
//! collateral pool) tested against a BATTERY of M market-wide price shocks. Each
//! scenario applies its shock to EVERY market in the portfolio (a correlated
//! crash), and the proven `risk::assess_margin` returns the WORST case across the
//! M scenarios. Reverts `Custom(112)` if the portfolio breaches maintenance under
//! any shock. Mutates NO state.
//!
//! This is the capstone risk probe: portfolio + stress in one atomic call, the
//! tool a keeper uses to answer "does this trader survive a 20% market-wide
//! drop?" across all their positions at once.
//!
//! accounts: [trader_state, <market, position> × N]  (N up to MAX_POSITIONS)
//! data: [m u8][ shock_bps i32 LE ; m ]   (1 ≤ m ≤ MAX_SCENARIOS)

use crate::instructions::margin_probe::build_snapshot;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

const MAX_POSITIONS: usize = 8;
const MAX_SCENARIOS: usize = 8;

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
    let [trader_state, pairs @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if pairs.len() % 2 != 0 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let pair_count = pairs.len() / 2;
    if pair_count > MAX_POSITIONS {
        return Err(ProgramError::InvalidInstructionData);
    }

    // shock battery: [m][i32 × m]
    let m = *data.first().ok_or(ProgramError::InvalidInstructionData)? as usize;
    if m == 0 || m > MAX_SCENARIOS {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data.len() < 1 + m * 4 {
        return Err(ProgramError::InvalidInstructionData);
    }

    // ── collect the portfolio snapshots (shared builder per pair) ───────
    let mut positions = [zero_position(); MAX_POSITIONS];
    let mut markets = [zero_market(); MAX_POSITIONS];
    let mut collateral: u64 = 0;
    let mut n = 0usize;
    for i in 0..pair_count {
        let market = &pairs[2 * i];
        let position = &pairs[2 * i + 1];
        if let Some((pos_snap, mkt_snap, c)) =
            build_snapshot(pid, market, trader_state, position, &[])?
        {
            // Re-audit 2026-06-30 (LOW parity): reject ISOLATED legs — this cross walk
            // pools one collateral but sums every leg's MM, so an isolated leg (own
            // bucket) would be charged here with its backing excluded → false-negative.
            // Iso positions verify via the single-position path (see liquidate_portfolio_v2).
            if pos_snap.collateral_quote_lots != 0 {
                return Err(ProgramError::InvalidArgument); // isolated leg — wrong probe
            }
            positions[n] = pos_snap;
            markets[n] = mkt_snap;
            collateral = c;
            n += 1;
        }
    }
    if n == 0 {
        return Ok(()); // all flat — survives any shock
    }

    // ── evaluate each shock as its own scenario (worst-of-M = "any breaches").
    //    One reused row of shocks (shock every market by bps[j]) keeps the SBF
    //    stack small — a full M×N grid would overflow the 4KB frame. ──────────
    let mut row = [StressShock {
        market: [0u8; 32],
        shock_bps: 0,
    }; MAX_POSITIONS];
    for j in 0..m {
        let off = 1 + j * 4;
        let shock_bps = i32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        for (k, cell) in row.iter_mut().enumerate().take(n) {
            *cell = StressShock {
                market: markets[k].market,
                shock_bps,
            };
        }
        let scenario: &[StressShock] = &row[..n];
        let assessment = assess_margin(&positions[..n], &markets[..n], &[scenario], collateral)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        if !assessment.is_healthy {
            return Err(ProgramError::Custom(112));
        }
    }
    Ok(())
}
