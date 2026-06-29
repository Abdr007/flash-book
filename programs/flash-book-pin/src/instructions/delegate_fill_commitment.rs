//! delegate_fill_commitment — delegate this market's fill-commitment ring PDA to
//! the ER (paired with delegate_market_book so the matcher commits fills on the
//! ER). Faithful port of the Anchor `delegate_fill_commitment`. Same shape as
//! delegate_market_book with the ring's `[b"fill_commit", market]` seeds.
//!
//! accounts: [authority (signer, payer), market (program-owned, r),
//!            fill_commitment (PDA owned by us, w, signs), owner_program (THIS
//!            program), delegate_buffer, delegation_record, delegation_metadata,
//!            system_program, delegation_program]
//! data: [commit_frequency_ms u32][has_validator u8][validator [u8;32] if has]

use crate::er::{
    cpi_delegate, write_delegate_data, DelegateAccounts, DELEGATE_BUFFER_TAG,
    DELEGATION_METADATA_TAG, DELEGATION_PROGRAM_ID, DELEGATION_RECORD_TAG, MAX_DELEGATE_DATA,
};
use crate::fill_commitment::FILL_COMMIT_SEED;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, fill_commitment, owner_program, delegate_buffer, delegation_record, delegation_metadata, system_program, delegation_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 5 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let commit_frequency_ms = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let has_validator = data[4] != 0;
    let mut validator = [0u8; 32];
    if has_validator {
        if data.len() < 37 {
            return Err(ProgramError::InvalidInstructionData);
        }
        validator.copy_from_slice(&data[5..37]);
    }

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

    // The ring PDA must be ours (not yet delegated).
    let bump = assert_pda(fill_commitment, &[FILL_COMMIT_SEED, &market.key()[..]], pid)?;
    if !fill_commitment.is_owned_by(pid) {
        return Err(ProgramError::Custom(210));
    }

    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    let fc = fill_commitment.key();
    if delegate_buffer.key() != &find_program_address(&[DELEGATE_BUFFER_TAG, &fc[..]], pid).0
        || delegation_record.key()
            != &find_program_address(&[DELEGATION_RECORD_TAG, &fc[..]], &DELEGATION_PROGRAM_ID).0
        || delegation_metadata.key()
            != &find_program_address(&[DELEGATION_METADATA_TAG, &fc[..]], &DELEGATION_PROGRAM_ID).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    let bump_arr = [bump];
    let mut buf = [0u8; MAX_DELEGATE_DATA];
    let len = write_delegate_data(
        &mut buf,
        commit_frequency_ms,
        &[FILL_COMMIT_SEED, &market.key()[..], &bump_arr[..]],
        if has_validator { Some(&validator) } else { None },
    );
    let seeds = [
        Seed::from(FILL_COMMIT_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_delegate(
        &DelegateAccounts {
            payer: authority,
            delegated_account: fill_commitment,
            owner_program,
            delegate_buffer,
            delegation_record,
            delegation_metadata,
            system_program,
            delegation_program,
        },
        &buf[..len],
        &signer,
    )
}
