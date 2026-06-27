//! set_market_params — the market authority updates mutable market parameters
//! (fees, rebate, lot/OI bounds). `tick_size` is INTENTIONALLY immutable here:
//! changing it would reprice every resting order, so it's fixed at init.
//!
//! Authority-gated (owner + disc + `market.authority == signer`). All params are
//! bounds-checked, exactly as at `initialize_market`.
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data (24 bytes LE): taker_fee_bps u32, maker_rebate_bps i32,
//!                     min_base_lots u64, max_oi_base_lots u64

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 24 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let taker_fee_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let maker_rebate_bps = i32::from_le_bytes(data[4..8].try_into().unwrap());
    let min_base_lots = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let max_oi_base_lots = u64::from_le_bytes(data[16..24].try_into().unwrap());

    // ── parameter bounds (same as initialize_market) ───────────────────
    if min_base_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if taker_fee_bps > crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }
    if maker_rebate_bps < 0 || (maker_rebate_bps as u32) > taker_fee_bps {
        return Err(ProgramError::InvalidArgument);
    }
    if max_oi_base_lots < min_base_lots {
        return Err(ProgramError::InvalidArgument);
    }

    // ── authority gate ──────────────────────────────────────────────────
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
        m.taker_fee_bps = taker_fee_bps;
        m.maker_rebate_bps = maker_rebate_bps;
        m.min_base_lots = min_base_lots;
        m.max_oi_base_lots = max_oi_base_lots;
    }
    Ok(())
}
