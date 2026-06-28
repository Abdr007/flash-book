//! verify_side_accrual_invariants — READ-ONLY consistency check on a market's
//! side-accrual (ADL) state. Asserts, for BOTH sides, that the ADL multiplier
//! `a` never exceeds the identity `ADL_ONE` (it is monotonically non-increasing
//! within an epoch, reset to `ADL_ONE`) and that `mode` is a valid `SideMode`
//! (0 = Normal, 1 = DrainOnly, 2 = ResetPending). Reverts `Custom(124)` on any
//! breach. Mutates NO state.
//!
//! Port-addition (the anchor program has no standalone side-accrual verify): the
//! same enforcing-probe pattern as `verify_haircut_invariants`, giving keepers an
//! on-chain check on the ADL bookkeeping the matching-side accrual will maintain.
//!
//! accounts: [market, side_accrual (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::seeds::SIDE_ACCRUAL_SEED;
use crate::side_accrual::ADL_ONE;
use crate::state::{MarketSideAccrual, SIDE_ACCRUAL_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Highest valid `SideMode` value (ResetPending).
const MAX_SIDE_MODE: u8 = 2;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, side_accrual, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(side_accrual, pid)?;
    assert_pda(side_accrual, &[SIDE_ACCRUAL_SEED, &market.key()[..]], pid)?;
    assert_disc(side_accrual, &SIDE_ACCRUAL_DISC)?;

    let ok = {
        let d = side_accrual.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const MarketSideAccrual) };
        if &s.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let long_a = u128::from_le_bytes(s.long_a);
        let short_a = u128::from_le_bytes(s.short_a);
        long_a <= ADL_ONE
            && short_a <= ADL_ONE
            && s.long_mode <= MAX_SIDE_MODE
            && s.short_mode <= MAX_SIDE_MODE
    };

    if !ok {
        return Err(ProgramError::Custom(124));
    }
    Ok(())
}
