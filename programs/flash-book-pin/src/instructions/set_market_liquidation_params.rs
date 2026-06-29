//! set_market_liquidation_params — the market authority configures the
//! `liquidate_position_v2` parameters: the synthetic-close penalty, the
//! Dutch-auction liquidator reward + its ramp duration, and the re-liquidation
//! cooldown. Authority-gated, bounds-checked (the two bps < BPS_DENOM).
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: [liq_penalty_bps u32][liquidator_reward_bps u32]
//!       [liquidation_auction_duration_slots u64][liquidation_cooldown_slots u64]

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
    let liq_penalty_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let liquidator_reward_bps = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let auction_duration_slots = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let cooldown_slots = u64::from_le_bytes(data[16..24].try_into().unwrap());
    // A penalty / reward ≥ 100% is nonsensical (would zero or invert collateral).
    if liq_penalty_bps >= crate::constants::BPS_DENOM
        || liquidator_reward_bps >= crate::constants::BPS_DENOM
    {
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
        m.liq_penalty_bps = liq_penalty_bps;
        m.liquidator_reward_bps = liquidator_reward_bps;
        m.liquidation_auction_duration_slots = auction_duration_slots;
        m.liquidation_cooldown_slots = cooldown_slots;
    }
    Ok(())
}
