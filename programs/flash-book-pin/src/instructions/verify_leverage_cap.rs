//! verify_leverage_cap — READ-ONLY check that a position respects its
//! `leverage_cap` (set via `set_position_leverage`). Computes the position's
//! notional and reverts `Custom(113)` if it exceeds `cap × cross collateral` —
//! i.e. the position's leverage is over its cap. Mutates NO state.
//!
//! This makes the per-position cap useful before order-placement enforcement
//! exists: a keeper can flag any position trading above its declared leverage.
//! Cross-margin scope (collateral = the trader_state pool), matching
//! `verify_solvency`. `cap == 0` (unset) and flat positions are trivially OK.
//!
//! accounts: [market, trader_state, position]

use crate::guard::{assert_disc, assert_market, assert_owned_by};
use crate::instructions::apply_fill::assert_position;
use crate::leverage::exceeds_leverage_cap;
use crate::state::{Market, Position, TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_position(position, pid)?;

    unsafe {
        let m = &*(market.borrow_data_unchecked().as_ptr() as *const Market);
        let ts = &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState);
        let p = &*(position.borrow_data_unchecked().as_ptr() as *const Position);

        // Flat or no cap set → trivially within cap.
        if p.size_lots == 0 || p.leverage_cap == 0 {
            return Ok(());
        }
        // Bind the position to THIS trader_state + market.
        if p.trader != ts.trader || p.market != *market.key() {
            return Err(ProgramError::InvalidArgument);
        }

        let notional = (p.size_lots as u128)
            .checked_mul(m.mark_price_ticks as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(m.tick_size as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if exceeds_leverage_cap(notional, p.leverage_cap, ts.collateral_quote_lots) {
            return Err(ProgramError::Custom(113));
        }
    }
    Ok(())
}
