//! set_trader_builder — the trader sets (or clears) a builder-code recipient and
//! the max bps of net fee it may receive. Trader-signed. Clearing the builder
//! (all-zero) forces the share to 0 so a stale share can't apply to "no builder".
//!
//! accounts: [trader (signer), trader_state (program-owned, w)]
//! data: builder (32-byte pubkey) | max_fee_share_bps (u32 LE)  — 36 bytes

use crate::constants::BPS_DENOM;
use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 36 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut builder = [0u8; 32];
    builder.copy_from_slice(&data[0..32]);
    let max_fee_share_bps = u32::from_le_bytes(data[32..36].try_into().unwrap());
    if max_fee_share_bps > BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(trader)?;
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        if &ts.trader != trader.key() {
            return Err(ProgramError::IllegalOwner);
        }
        ts.builder = builder;
        // No builder ⇒ no share, regardless of the supplied bps.
        ts.builder_max_fee_share_bps = if builder == [0u8; 32] { 0 } else { max_fee_share_bps };
    }
    Ok(())
}
