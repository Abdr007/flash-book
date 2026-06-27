//! update_fee_tiers — replace the singleton fee-tier table in place. Gated on the
//! recorded `fee_tiers.authority`. Re-validated against the same rules as init.
//!
//! accounts: [authority (signer), fee_tiers (PDA, program-owned, w)]
//! data: [volume_window_slots u64][tier_count u8]
//!     [ (min_volume u64)(maker_rebate i32)(taker_fee u32) ; tier_count ]

use crate::fee_tiers::{parse_fee_tiers, validate_fee_tiers};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::FEE_TIERS_SEED;
use crate::state::{FeeTier, FeeTiers, FEE_TIERS_DISC, MAX_FEE_TIERS};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, fee_tiers, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let window = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let count = data[8] as usize;

    let mut buf = [(0u64, 0i32, 0u32); MAX_FEE_TIERS];
    let n = parse_fee_tiers(count, &data[9..], &mut buf)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    validate_fee_tiers(window, &buf[..n]).map_err(|_| ProgramError::InvalidArgument)?;

    // ── authority gate (genuine singleton PDA + recorded authority) ─────
    assert_signer(authority)?;
    assert_owned_by(fee_tiers, program_id)?;
    assert_pda(fee_tiers, &[FEE_TIERS_SEED], program_id)?;
    assert_disc(fee_tiers, &FEE_TIERS_DISC)?;
    {
        let d = fee_tiers.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const FeeTiers) };
        if &t.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    unsafe {
        let t = &mut *(fee_tiers.borrow_mut_data_unchecked().as_mut_ptr() as *mut FeeTiers);
        t.tier_count = n as u8;
        t.volume_window_slots = window;
        t.tiers = [FeeTier { min_volume_quote_lots: 0, maker_rebate_bps: 0, taker_fee_bps: 0 };
            MAX_FEE_TIERS];
        for (i, &(min_vol, maker, taker)) in buf[..n].iter().enumerate() {
            t.tiers[i] = FeeTier {
                min_volume_quote_lots: min_vol,
                maker_rebate_bps: maker,
                taker_fee_bps: taker,
            };
        }
    }
    Ok(())
}
