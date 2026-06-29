//! attest_er_reserved_margin — the pinned ER attestor reports the trader's
//! ER-reserved initial margin (and bumps the attestation epoch). Only the
//! `attestor` recorded at init may call; the epoch is a STRICTLY-increasing
//! replay guard, so a stale/replayed attestation is rejected. Mutates the
//! attestation account AND denormalizes `er_active` onto the trader_state — NO
//! funds, NO book. The strict withdraw/sweep paths fail closed for ER-active
//! traders (forcing the `*_xdomain` variants that honor `reserved_margin`), so
//! this flag is load-bearing: parity with the Anchor original, which sets
//! `s.er_active = if reserved > 0 { 1 } else { 0 }` here.
//!
//! accounts: [attestor (signer), er_margin (PDA, program-owned, w),
//!            trader_state (program-owned, w)]
//! data: [reserved_margin_quote_lots u64][epoch u64]

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::ER_MARGIN_SEED;
use crate::state::{ErMarginAttestation, TraderState, ER_MARGIN_DISC, TRADER_STATE_DISC};
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

    // Denormalize onto the trader_state so the strict withdraw/sweep paths can
    // fail closed without loading the attestation (parity with Anchor). The
    // trader_state must be writable; the attestor is trusted to pass it `w`.
    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.er_active = if reserved_margin_quote_lots > 0 { 1 } else { 0 };
    }
    Ok(())
}
