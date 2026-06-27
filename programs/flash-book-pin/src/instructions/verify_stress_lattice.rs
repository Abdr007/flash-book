//! verify_stress_lattice — READ-ONLY stress test of a position against a BATTERY
//! of N price shocks in a single call. Where `verify_stress_solvency` checks one
//! shock, this builds N single-shock scenarios and runs the proven
//! `risk::assess_margin`, which reports the WORST case across them. Reverts
//! `Custom(111)` if the position would breach maintenance under any supplied
//! shock. Mutates NO state.
//!
//! This is flash-book's stress-lattice probe: a keeper passes a ladder of adverse
//! moves (e.g. -5%, -10%, -20%, -35%) and the call fails if the position can't
//! survive the worst rung — all in one atomic, CU-bounded transaction.
//!
//! accounts: [market, trader_state, position, (leverage_tiers — optional)]
//! data: [n u8][ shock_bps i32 LE ; n ]   (1 ≤ n ≤ MAX_SCENARIOS)

use crate::instructions::margin_probe::build_snapshot;
use crate::risk::{assess_margin, StressShock};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Cap on shocks per call (bounds CU + the stack scenario arrays).
const MAX_SCENARIOS: usize = 16;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let n = *data.first().ok_or(ProgramError::InvalidInstructionData)? as usize;
    if n == 0 || n > MAX_SCENARIOS {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data.len() < 1 + n * 4 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let Some((pos_snap, mkt_snap, collateral)) =
        build_snapshot(pid, market, trader_state, position, rest)?
    else {
        return Ok(()); // flat position — survives any shock
    };

    // One StressShock per supplied bps, each on THIS market. Then one scenario
    // per shock so `assess_margin` takes the worst case across the lattice.
    let mut shocks = [StressShock {
        market: mkt_snap.market,
        shock_bps: 0,
    }; MAX_SCENARIOS];
    for (i, slot) in shocks.iter_mut().enumerate().take(n) {
        let off = 1 + i * 4;
        slot.shock_bps = i32::from_le_bytes(data[off..off + 4].try_into().unwrap());
    }
    let mut scenarios: [&[StressShock]; MAX_SCENARIOS] = [&[]; MAX_SCENARIOS];
    for (i, scen) in scenarios.iter_mut().enumerate().take(n) {
        *scen = core::slice::from_ref(&shocks[i]);
    }

    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &scenarios[..n], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        return Err(ProgramError::Custom(111));
    }
    Ok(())
}
