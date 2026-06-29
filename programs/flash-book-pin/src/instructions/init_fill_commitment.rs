//! init_fill_commitment — one-time per market: create the FillCommitmentAccount
//! ring (`[b"fill_commit", market]`) and ARM the market so settlement requires a
//! matching committed fill (`fill_commitment_required = 1`, sticky). Faithful
//! port of the Anchor `init_fill_commitment`. Authority-gated.
//!
//! accounts: [authority (signer, payer), market (program-owned, w),
//!            fill_commitment (PDA, w, uninit), system_program]
//! data: (none — capacity is the default FILL_RING_CAP)

use crate::cpi::create_pda_account;
use crate::fill_commitment::{
    buffer_init, fill_commit_account_len, FILL_COMMIT_SEED, FILL_RING_CAP,
};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer, assert_uninitialized};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

/// Offset of `Market.fill_commitment_required` (after `book_delegated_at_slot`).
const MKT_FILL_COMMIT_REQUIRED: usize = 240;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, fill_commitment, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // Create the ring PDA.
    assert_uninitialized(fill_commitment)?;
    let bump = assert_pda(fill_commitment, &[FILL_COMMIT_SEED, &market.key()[..]], pid)?;
    let space = fill_commit_account_len(FILL_RING_CAP as usize);
    let lamports = Rent::get()?.minimum_balance(space);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(FILL_COMMIT_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(authority, fill_commitment, system_program, lamports, space as u64, pid, &signer)?;

    // Stamp the ring header.
    {
        let mut d = fill_commitment.try_borrow_mut_data()?;
        buffer_init(&mut d, market.key(), FILL_RING_CAP, bump).map_err(|_| ProgramError::InvalidAccountData)?;
    }

    // Arm the market (sticky): settlement now requires the ring.
    unsafe {
        let m = market.borrow_mut_data_unchecked();
        m[MKT_FILL_COMMIT_REQUIRED] = 1;
    }
    Ok(())
}
