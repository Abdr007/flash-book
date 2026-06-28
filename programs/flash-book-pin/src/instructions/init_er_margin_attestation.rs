//! init_er_margin_attestation — create a trader's ER margin attestation account,
//! PDA `[b"er_margin", trader_state]`. Protocol-authority gated (the insurance
//! fund's authority); pins the `attestor` (the only key allowed to update the
//! attestation later). Created empty (reserved_margin = 0, epoch = 0). NO funds,
//! NO book. The attest/update + withdraw-gate consumers are later batches.
//!
//! accounts: [authority (signer, payer, w), insurance (PDA, owned, r),
//!            trader_state (program-owned, r), er_margin (PDA, w, uninit),
//!            system_program]
//! data: attestor (32-byte pubkey)

use crate::cpi::create_pda_account;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{ER_MARGIN_SEED, INSURANCE_SEED};
use crate::state::{
    ErMarginAttestation, Insurance, ER_MARGIN_DISC, INSURANCE_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const ER_MARGIN_LEN: usize = core::mem::size_of::<ErMarginAttestation>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, insurance, trader_state, er_margin, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut attestor = [0u8; 32];
    attestor.copy_from_slice(&data[0..32]);

    // ── auth: protocol admin (insurance authority) ──────────────────────
    assert_signer(authority)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // The attestation binds to a genuine trader_state.
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    // ── create the PDA ──────────────────────────────────────────────────
    let bump = assert_pda(
        er_margin,
        &[ER_MARGIN_SEED, &trader_state.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(ER_MARGIN_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(ER_MARGIN_SEED),
        Seed::from(&trader_state.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        er_margin,
        system_program,
        lamports,
        ER_MARGIN_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let a = &mut *(er_margin.borrow_mut_data_unchecked().as_mut_ptr() as *mut ErMarginAttestation);
        a.disc = ER_MARGIN_DISC;
        a.trader_state = *trader_state.key();
        a.attestor = attestor;
        a.reserved_margin_quote_lots = 0;
        a.epoch = 0;
        a.bump = bump;
        a._pad = [0u8; 7];
    }
    Ok(())
}
