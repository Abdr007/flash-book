//! verify_fee_tiers — READ-ONLY consistency check on the singleton fee-tier
//! table. Re-runs the host-tested `fee_tiers::validate_fee_tiers` over the STORED
//! ladder, catching a corrupted table (window 0, first rung not zero-volume,
//! non-monotone fee/rebate, out-of-bounds magnitudes). Reverts `Custom(127)` on
//! a breach. Mutates NO state.
//!
//! Port-addition (no standalone fee-tier verify in anchor): re-uses the exact
//! write-time validator, same enforcing-probe shape as the other `verify_*`.
//!
//! accounts: [fee_tiers (PDA, program-owned, r)]

use crate::fee_tiers::validate_fee_tiers;
use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::FEE_TIERS_SEED;
use crate::state::{FeeTiers, FEE_TIERS_DISC, MAX_FEE_TIERS};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [fee_tiers, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_owned_by(fee_tiers, pid)?;
    assert_pda(fee_tiers, &[FEE_TIERS_SEED], pid)?;
    assert_disc(fee_tiers, &FEE_TIERS_DISC)?;

    let mut buf = [(0u64, 0i32, 0u32); MAX_FEE_TIERS];
    let (window, count) = {
        let d = fee_tiers.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const FeeTiers) };
        let n = (t.tier_count as usize).min(MAX_FEE_TIERS);
        for (i, slot) in buf.iter_mut().enumerate().take(n) {
            *slot = (
                t.tiers[i].min_volume_quote_lots,
                t.tiers[i].maker_rebate_bps,
                t.tiers[i].taker_fee_bps,
            );
        }
        (t.volume_window_slots, n)
    };

    validate_fee_tiers(window, &buf[..count]).map_err(|_| ProgramError::Custom(127))?;
    Ok(())
}
