//! settle_funding (cross-margin path). Pointer-casts market/trader_state/
//! position, applies funding owed to collateral, advances the position's
//! funding index. Isolated-bucket + haircut routing are TODO (documented).
use crate::funding::funding_owed;
use crate::state::{Market, Position, TraderState};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

#[inline(always)]
unsafe fn view<T>(ai: &AccountInfo) -> &mut T { &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T) }

/// accounts: [market, trader_state, position]
pub fn process(_pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 3 { return Err(ProgramError::NotEnoughAccountKeys); }
    unsafe {
        let market: &Market = view(&accounts[0]);
        let trader_state: &mut TraderState = view(&accounts[1]);
        let position: &mut Position = view(&accounts[2]);
        let cum_now = market.cum_funding();
        if position.size_lots == 0 { position.set_cum_funding(cum_now); return Ok(()); }
        let notional = (position.size_lots as u128)
            .checked_mul(market.mark_price_ticks as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(market.tick_size as u128).ok_or(ProgramError::ArithmeticOverflow)?;
        if notional > u64::MAX as u128 { return Err(ProgramError::ArithmeticOverflow); }
        let owed = funding_owed(position.side == 0, notional as u64, cum_now, position.cum_funding())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // cross path: collateral -= owed, floored at 0.
        let new_c = (trader_state.collateral_quote_lots as i128 - owed).max(0);
        trader_state.collateral_quote_lots = new_c as u64;
        position.set_cum_funding(cum_now);
    }
    Ok(())
}
