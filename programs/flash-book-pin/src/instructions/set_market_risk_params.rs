//! set_market_risk_params — the market authority configures the optional MMR
//! surcharges the risk engine applies on top of the base maintenance margin:
//! a concentration penalty (large single positions) and an OI-scaled penalty
//! (crowded same-side books). Authority-gated. All-zero (the carved default)
//! means "no surcharge", so a market that never calls this behaves exactly as
//! before.
//!
//! `verify_solvency` reads these into its `MarketSnapshot`; the math lives in the
//! proven `risk::effective_mmr_bps`.
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: concentration_threshold_lots (u64 LE)
//!     | concentration_extra_mmr_bps (u32 LE)
//!     | oi_mmr_slope_bps_per_million_lots (u32 LE)
//!     | oi_mmr_max_extra_bps (u32 LE)   — 20 bytes

use crate::constants::BPS_DENOM;
use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 20 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let concentration_threshold_lots = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let concentration_extra_mmr_bps = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let oi_mmr_slope_bps_per_million_lots = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let oi_mmr_max_extra_bps = u32::from_le_bytes(data[16..20].try_into().unwrap());

    // A single surcharge that adds unconditionally must stay within 100%. The
    // OI cap (`oi_mmr_max_extra_bps`) is intentionally NOT bounded here: `0` is
    // the "uncapped" sentinel the risk math relies on, and the slope governs how
    // fast it grows. Concentration extra is the one applied as a flat add.
    if concentration_extra_mmr_bps > BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(authority)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.concentration_threshold_lots = concentration_threshold_lots;
        m.concentration_extra_mmr_bps = concentration_extra_mmr_bps;
        m.oi_mmr_slope_bps_per_million_lots = oi_mmr_slope_bps_per_million_lots;
        m.oi_mmr_max_extra_bps = oi_mmr_max_extra_bps;
    }
    Ok(())
}
