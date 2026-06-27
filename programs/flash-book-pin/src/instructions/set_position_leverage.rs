//! set_position_leverage — the trader sets a per-position max-leverage cap,
//! bounded by `Market::max_leverage`. `0` clears the cap. Trader-signed; the
//! position is bound to the signer and the paired market.
//!
//! accounts: [trader (signer), market (program-owned, r), position (program-owned, w)]
//! data: cap (u32 LE)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, Position, MARKET_DISC, POSITION_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, market, position, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let cap = u32::from_le_bytes(data[0..4].try_into().unwrap());

    assert_signer(trader)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    assert_owned_by(position, program_id)?;
    assert_disc(position, &POSITION_DISC)?;

    // Bound the cap by the market max (0 on either side = unbounded).
    let max_leverage = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        m.max_leverage
    };
    if cap > 0 && max_leverage > 0 && cap > max_leverage {
        return Err(ProgramError::InvalidArgument);
    }

    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut Position);
        // The position must be the signer's, on the supplied market.
        if &p.trader != trader.key() || &p.market != market.key() {
            return Err(ProgramError::IllegalOwner);
        }
        p.leverage_cap = cap;
    }
    Ok(())
}
