//! verify_haircut_invariants — READ-ONLY internal-consistency check on a market's
//! haircut state. Runs the host-tested `haircut::verify_invariants` over the
//! stored accumulators and reverts `Custom(121)` if any invariant fails. Mutates
//! NO state.
//!
//! (The anchor counterpart only emits a pass/bitmask event; the port has no
//! events, so it ENFORCES instead — a keeper polls it and pages on revert, same
//! as the other `verify_*` probes.)
//!
//! accounts: [market, haircut_state (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::haircut::verify_invariants;
use crate::seeds::HAIRCUT_SEED;
use crate::state::{MarketHaircutState, HAIRCUT_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, haircut_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(haircut_state, pid)?;
    assert_pda(haircut_state, &[HAIRCUT_SEED, &market.key()[..]], pid)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;

    let report = {
        let d = haircut_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const MarketHaircutState) };
        if &s.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        verify_invariants(
            u128::from_le_bytes(s.residual_quote_lots),
            u128::from_le_bytes(s.matured_pos_total_quote_lots),
            u128::from_le_bytes(s.realized_loss_total_quote_lots),
            u128::from_le_bytes(s.dust_accrued_quote_lots),
            s.h_min_slots,
            s.h_max_slots,
            s.h_scaled_cached,
            s.h_cached_at_slot,
        )
    };

    if !report.all_ok() {
        return Err(ProgramError::Custom(121));
    }
    Ok(())
}
