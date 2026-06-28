//! attest_er_reserved_margin — the pinned ER attestor reports the trader's
//! ER-reserved initial margin (and bumps the attestation epoch). Only the
//! `attestor` recorded at init may call; the epoch is a STRICTLY-increasing
//! replay guard, so a stale/replayed attestation is rejected. Mutates only the
//! attestation account — NO funds, NO book. The withdraw gate that honors
//! `reserved_margin` (and reads it directly, so no `er_active` denormalization is
//! needed) is a later batch.
//!
//! accounts: [attestor (signer), er_margin (PDA, program-owned, w),
//!            trader_state (program-owned, r)]
//! data: [reserved_margin_quote_lots u64][epoch u64]

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::ER_MARGIN_SEED;
use crate::state::{ErMarginAttestation, ER_MARGIN_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [attestor, er_margin, trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let reserved_margin_quote_lots = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let epoch = u64::from_le_bytes(data[8..16].try_into().unwrap());

    assert_signer(attestor)?;
    assert_owned_by(er_margin, program_id)?;
    assert_pda(er_margin, &[ER_MARGIN_SEED, &trader_state.key()[..]], program_id)?;
    assert_disc(er_margin, &ER_MARGIN_DISC)?;
    // The trader_state must be a genuine one (the attestation binds to it).
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    unsafe {
        let a = &mut *(er_margin.borrow_mut_data_unchecked().as_mut_ptr() as *mut ErMarginAttestation);
        // Only the pinned attestor, bound to this trader_state.
        if &a.attestor != attestor.key() {
            return Err(ProgramError::IllegalOwner);
        }
        if &a.trader_state != trader_state.key() {
            return Err(ProgramError::InvalidArgument);
        }
        // Strictly-increasing epoch (monotonic replay guard).
        if epoch <= a.epoch {
            return Err(ProgramError::InvalidArgument);
        }
        a.epoch = epoch;
        a.reserved_margin_quote_lots = reserved_margin_quote_lots;
    }
    Ok(())
}
