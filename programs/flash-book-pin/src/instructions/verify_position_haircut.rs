//! verify_position_haircut — READ-ONLY consistency check on a position's haircut
//! (PnL warmup) state. Asserts the documented invariants of the warmup
//! accumulators and reverts `Custom(129)` on a breach. Mutates NO state.
//!
//! Conservatively asserts only what is unambiguously invariant (so a CORRECT
//! state can never false-revert):
//!  * `original_reserve_at_attach ≥ released_reserve_quote_lots` — the un-matured
//!    reserve never exceeds the total reserved at warmup start (it only drains);
//!  * `released_reserve_quote_lots == 0 ⇒ released_attached_at_slot == 0` — the
//!    attach slot is cleared when the reserve fully drains.
//!
//! Port-addition; same enforcing-probe shape as `verify_haircut_invariants`.
//!
//! accounts: [position_haircut (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::POSITION_HAIRCUT_SEED;
use crate::state::{PositionHaircutState, POSITION_HAIRCUT_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [position_haircut, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_owned_by(position_haircut, pid)?;
    assert_disc(position_haircut, &POSITION_HAIRCUT_DISC)?;

    let (market, position, released_reserve, attached_slot, original_reserve) = {
        let d = position_haircut.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const PositionHaircutState) };
        (
            p.market,
            p.position,
            p.released_reserve_quote_lots,
            p.released_attached_at_slot,
            p.original_reserve_at_attach,
        )
    };
    // Confirm it sits at its canonical PDA for (market, position).
    assert_pda(
        position_haircut,
        &[POSITION_HAIRCUT_SEED, &market[..], &position[..]],
        pid,
    )?;

    let ok = original_reserve >= released_reserve
        && (released_reserve != 0 || attached_slot == 0);
    if !ok {
        return Err(ProgramError::Custom(129));
    }
    Ok(())
}
