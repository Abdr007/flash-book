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
use crate::funding::funding_owed;
use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::haircut::apply_residual_delta;
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
        // funding against another trader's collateral.
        if position.trader != trader_state.trader { return Err(ProgramError::InvalidArgument); }
        if position.market != *accounts[0].key() { return Err(ProgramError::InvalidArgument); }
        let notional = (position.size_lots as u128)
            .checked_mul(market.mark_price_ticks as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(market.tick_size as u128).ok_or(ProgramError::ArithmeticOverflow)?;
        if notional > u64::MAX as u128 { return Err(ProgramError::ArithmeticOverflow); }
        let owed = funding_owed(position.side == 0, notional as u64, cum_now, position.cum_funding())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // Clamp owed to i64 range; rounded values that overflow i64 are capped
        // (only reachable with insane funding rates) — matches anchor. (The
        // prior code did `(collateral - owed).max(0) as u64`, which WRAPS when
        // a received credit pushes collateral past u64::MAX — silently
        // destroying the credit.)
        let owed_i64: i64 = if owed > i64::MAX as i128 { i64::MAX }
            else if owed < i64::MIN as i128 { i64::MIN }
            else { owed as i64 };

        // Isolated position (per-position bucket funded) settles to/from that
        // bucket; a cross position settles to/from the pooled trader collateral.
        let is_isolated = position.collateral_quote_lots > 0;
        // Track the ACTUAL collateral moved (clamped to availability) so the
        // residual delta matches the real change in committed collateral.
        let mut paid: u64 = 0;
        let mut received: u64 = 0;
        if owed_i64 > 0 {
            let owed_u64 = owed_i64 as u64;
            if is_isolated {
                paid = owed_u64.min(position.collateral_quote_lots);
                position.collateral_quote_lots -= paid;
            } else {
                paid = owed_u64.min(trader_state.collateral_quote_lots);
                trader_state.collateral_quote_lots -= paid;
            }
        } else if owed_i64 < 0 {
            received = owed_i64.unsigned_abs();
            if is_isolated {
                position.collateral_quote_lots = position.collateral_quote_lots
                    .checked_add(received).ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                trader_state.collateral_quote_lots = trader_state.collateral_quote_lots
                    .checked_add(received).ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }

        // RISK-1: move the solvency residual by the collateral actually moved.
        // paid ⇒ residual ↑ ; received ⇒ residual ↓ (underflow ⇒ insolvency,
        // rejected by the checked sub inside apply_residual_delta).
        if paid > 0 || received > 0 {
            let delta: i128 = paid as i128 - received as i128;
            let current = u128::from_le_bytes(haircut.residual_quote_lots);
            let new_residual = apply_residual_delta(current, delta)
                .map_err(|_| ProgramError::InvalidArgument)?;
            haircut.residual_quote_lots = new_residual.to_le_bytes();
        }
        position.set_cum_funding(cum_now);
    }
    Ok(())
}
