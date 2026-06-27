//! verify_stress_solvency — READ-ONLY stress probe. Like `verify_solvency`, but
//! prices the position's maintenance requirement under a caller-supplied price
//! SHOCK (signed bps) instead of at the current mark. Reverts `Custom(110)` if
//! the position would breach maintenance under that shock — the on-chain
//! building block for stress-lattice monitoring and (later) stress-gated
//! liquidation. Mutates NO state.
//!
//! The shock is applied as given (a keeper passes the adverse direction: negative
//! bps to stress a long, positive to stress a short). `assess_margin` reports the
//! WORST case across the scenarios it is given — here, the single supplied shock.
//!
//! accounts: [market, trader_state, position, (leverage_tiers — optional)]
//! data: shock_bps (i32 LE)

use crate::instructions::margin_probe::build_snapshot;
use crate::risk::{assess_margin, StressShock};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let shock_bps = i32::from_le_bytes(data[0..4].try_into().unwrap());

    let Some((pos_snap, mkt_snap, collateral)) =
        build_snapshot(pid, market, trader_state, position, rest)?
    else {
        return Ok(()); // flat position — trivially solvent under any shock
    };

    // One scenario: the supplied shock on THIS market (matched by key in
    // `shock_for_market`). `shocked_price` clamps a wipe-out shock at 0.
    let shocks = [StressShock {
        market: mkt_snap.market,
        shock_bps,
    }];
    let scenario: &[StressShock] = &shocks;
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[scenario], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        return Err(ProgramError::Custom(110));
    }
    Ok(())
}
