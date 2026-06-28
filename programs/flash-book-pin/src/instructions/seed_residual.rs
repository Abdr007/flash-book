//! seed_residual — the market authority adjusts the haircut residual (V − C − I,
//! the buffer that can back released positive PnL) by a SIGNED delta, via the
//! host-tested `haircut::apply_residual_delta` (checked add/sub, underflow-safe).
//! Mutates only the haircut state's residual accumulator. NO funds, NO book.
//!
//! accounts: [authority (signer), market (program-owned, r),
//!            haircut_state (PDA, program-owned, w)]
//! data: delta (i128 LE, 16 bytes)

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::haircut::apply_residual_delta;
use crate::seeds::HAIRCUT_SEED;
use crate::state::{Market, MarketHaircutState, HAIRCUT_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, haircut_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let delta = i128::from_le_bytes(data[0..16].try_into().unwrap());

    // ── auth: market authority ──────────────────────────────────────────
    assert_market(market, program_id)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        // The signer must be the market authority.
        if !authority.is_signer() || &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // ── the haircut state must be the market's canonical PDA ────────────
    assert_owned_by(haircut_state, program_id)?;
    assert_pda(haircut_state, &[HAIRCUT_SEED, &market.key()[..]], program_id)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;

    unsafe {
        let h = &mut *(haircut_state.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketHaircutState);
        if &h.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let current = u128::from_le_bytes(h.residual_quote_lots);
        let new_residual =
            apply_residual_delta(current, delta).map_err(|_| ProgramError::InvalidArgument)?;
        h.residual_quote_lots = new_residual.to_le_bytes();
    }
    Ok(())
}
