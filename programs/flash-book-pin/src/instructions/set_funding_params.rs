//! set_funding_params — market-authority configures the funding-rate engine: the
//! skew factor K (e9 funding per unit normalized OI skew), the ramp velocity (max
//! e9 rate change per slot), and the rate cap (saturating |rate|). All `0` (the
//! carved default) keeps funding INERT — so a market opts IN by setting non-zero
//! params. Bounded by `MAX_FUNDING_RATE_E9` so a mis-set market can't accrue
//! runaway funding. See `advance_funding` for the accrual crank.
//!
//! accounts: [authority (signer), market (PDA, w)]
//! data: [skew_factor_e9 u32][velocity_e9 u32][max_rate_e9 u32]  — 12 bytes

use crate::guard::{assert_market, assert_signer};
use crate::state::Market;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 12 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let skew_factor = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let velocity = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let max_rate = u32::from_le_bytes(data[8..12].try_into().unwrap());

    // Bound all three (e9, per slot) so a mis-set market can't accrue runaway funding.
    let cap = crate::constants::MAX_FUNDING_RATE_E9;
    if skew_factor > cap || velocity > cap || max_rate > cap {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(authority)?;
    assert_market(market, pid)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner); // only the market authority
        }
    }
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.funding_skew_factor_e9 = skew_factor.to_le_bytes();
        m.funding_velocity_e9 = velocity.to_le_bytes();
        m.max_funding_rate_e9 = max_rate.to_le_bytes();
    }
    Ok(())
}
