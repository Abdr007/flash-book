//! initialize_flp_exposure — create the protocol-singleton FLP exposure / NAV
//! account, PDA `[b"flp_exposure"]`. This is the pool-as-maker capital pool that
//! backs `apply_flp_fill`. The creator becomes its authority.
//!
//! Secure-by-default: creator signs, the account must be fresh, the PDA is
//! re-derived. No token movement here — capital deposits are a follow-up
//! (`deposit_flp_capital`).
//!
//! accounts: [authority (signer, payer, w), flp_exposure (PDA, w), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::FLP_EXPOSURE_SEED;
use crate::state::{FlpExposure, FLP_EXPOSURE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const FLP_EXPOSURE_LEN: usize = core::mem::size_of::<FlpExposure>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, flp_exposure, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_uninitialized(flp_exposure)?;
    let bump = assert_pda(flp_exposure, &[FLP_EXPOSURE_SEED], program_id)?;

    let lamports = Rent::get()?.minimum_balance(FLP_EXPOSURE_LEN);
    let bump_arr = [bump];
    let seeds = [Seed::from(FLP_EXPOSURE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        flp_exposure,
        system_program,
        lamports,
        FLP_EXPOSURE_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let f = &mut *(flp_exposure.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpExposure);
        f.disc = FLP_EXPOSURE_DISC;
        f.authority = *authority.key();
        f.total_capital_quote_lots = 0;
        f.realized_pnl = 0;
        f.lp_shares_outstanding = 0;
        f.bump = bump;
        f.markets_count = 0;
        // per_market[] is left zero-initialized (markets_count == 0 ⇒ none active).
    }
    Ok(())
}
