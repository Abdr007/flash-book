//! settle_funding (cross + isolated). Pointer-casts market/trader_state/
//! position/haircut_state, applies funding owed (entry-priced, lazy per
//! position) to the right collateral bucket, advances the position's funding
//! index, and — crucially — moves the haircut **solvency residual** by the
//! actual collateral that changed hands (RISK-1).
//!
//! Funding here is NOT zero-sum (it is entry-priced and settled lazily per
//! position), so a paid/received leg genuinely changes total committed trader
//! collateral `C_tot`, hence the solvency residual `V − C_tot − I`:
//!   trader PAYS    → C_tot ↓ → residual ↑  (more backing per claim)
//!   trader RECEIVES→ C_tot ↑ → residual ↓  (underflow ⇒ insolvency ⇒ reject)
//! Without this the funding path would silently mint/burn protocol collateral
//! and the haircut / kill-switch could not bound the drift. Faithful port of
//! the Anchor `settle_funding`.
use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::instructions::apply_fill::assert_position;
use crate::seeds::HAIRCUT_SEED;
use crate::state::{
    Market, MarketHaircutState, Position, TraderState, HAIRCUT_STATE_DISC, TRADER_STATE_DISC,
};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

#[inline(always)]
unsafe fn view<T>(ai: &AccountInfo) -> &mut T { &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T) }

/// accounts: [market, trader_state, position, haircut_state]
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 4 { return Err(ProgramError::NotEnoughAccountKeys); }
    // Hardening: validate ownership + discriminators before pointer-casting.
    assert_market(&accounts[0], pid)?;
    assert_owned_by(&accounts[1], pid)?; assert_disc(&accounts[1], &TRADER_STATE_DISC)?;
    assert_position(&accounts[2], pid)?;
    // The haircut state must be the market's canonical PDA (so the residual we
    // move is THIS market's accumulator, not an attacker-supplied account).
    assert_owned_by(&accounts[3], pid)?;
    assert_pda(&accounts[3], &[HAIRCUT_SEED, &accounts[0].key()[..]], pid)?;
    assert_disc(&accounts[3], &HAIRCUT_STATE_DISC)?;
    unsafe {
        let market: &Market = view(&accounts[0]);
        let trader_state: &mut TraderState = view(&accounts[1]);
        let position: &mut Position = view(&accounts[2]);
        let haircut: &mut MarketHaircutState = view(&accounts[3]);
        if &haircut.market != accounts[0].key() { return Err(ProgramError::InvalidArgument); }
        let cum_now = market.cum_funding();
        if position.size_lots == 0 { position.set_cum_funding(cum_now); return Ok(()); }
        // Bind the position to THIS trader_state + market — settle is
        // permissionless, so without this anyone could apply one trader's
        // funding against another trader's collateral. Bind by `sub_index` too
        // (not just the wallet `.trader`): otherwise a cross trader passes a
        // funded position + a DIFFERENT empty sub-account's trader_state, so the
        // clamp-pay debits 0 yet `cum_funding` is re-stamped — ERASING the funding
        // obligation without payment and drifting the solvency residual into bad
        // debt. Parity with apply_fill's bind + anchor's per-trader_state position PDA.
        if position.trader != trader_state.trader { return Err(ProgramError::InvalidArgument); }
        if position.market != *accounts[0].key() { return Err(ProgramError::InvalidArgument); }
        if position.sub_index != trader_state.sub_index { return Err(ProgramError::InvalidArgument); }
        // Shared funding-settle math (the SAME helper apply_fill/apply_flp_fill
        // use inline before a resize): settle on the current size against
        // `cum_now`, fold into the isolated/cross bucket, move the residual
        // (Δcollateral == −Δresidual), re-stamp. Single implementation ⇒ the
        // crank and the inline settle can never diverge.
        let mut residual = u128::from_le_bytes(haircut.residual_quote_lots);
        crate::funding::settle_position_funding(
            position,
            market.mark_price_ticks,
            market.tick_size,
            cum_now,
            &mut trader_state.collateral_quote_lots,
            &mut residual,
        )
        .map_err(|_| ProgramError::InvalidArgument)?;
        haircut.residual_quote_lots = residual.to_le_bytes();
    }
    Ok(())
}
