//! verify_solvency — READ-ONLY maintenance-margin check on a single position.
//!
//! Builds the position + market snapshots (via the shared `margin_probe`) and
//! runs the proven `risk::assess_margin` against a single ZERO-shock scenario, so
//! the position's equity is measured against its actual maintenance requirement
//! (not merely equity ≥ 0). Succeeds iff `available ≥ required`; errors
//! `Custom(100)` otherwise. Mutates NO state.
//!
//! Tiered MMR + concentration/OI surcharges are applied by the shared builder; an
//! optional `leverage_tiers` account resolves the position's tier.
//!
//! accounts: [market, trader_state, position, (leverage_tiers — optional)]

use crate::instructions::margin_probe::build_snapshot;
use crate::risk::{assess_margin, StressShock};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let Some((pos_snap, mkt_snap, collateral)) =
        build_snapshot(pid, market, trader_state, position, rest)?
    else {
        return Ok(()); // flat position — trivially solvent
    };

    // A single ZERO-shock scenario so the MAINTENANCE requirement is actually
    // evaluated (an empty scenario set would only check equity ≥ 0).
    // `shocked_price(p, 0) == p`, pricing the base maintenance margin at mark.
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[no_shock], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        return Err(ProgramError::Custom(100));
    }
    Ok(())
}
