//! delegate_market — delegate the Market PDA itself to the ER (paired with
//! delegate_market_book: the matcher mutates mark/funding/batch state on the ER
//! in lockstep with the book). Faithful port of the Anchor `delegate_market`.
//! Authority-gated; the market PDA signs the Delegate CPI as itself.
//!
//! pin's Market doesn't store its base/quote mint or bump, so the two mint
//! accounts are passed to re-derive the canonical PDA + its signer seeds.
//!
//! accounts: [authority (signer, payer), market (PDA owned by us, w, signs),
//!            base_mint (r), quote_mint (r), owner_program (THIS program),
//!            delegate_buffer, delegation_record, delegation_metadata,
//!            system_program, delegation_program]
//! data: [commit_frequency_ms u32][has_validator u8][validator [u8;32] if has]

use crate::er::{
    cpi_delegate, write_delegate_data, DelegateAccounts, DELEGATE_BUFFER_TAG,
    DELEGATION_METADATA_TAG, DELEGATION_PROGRAM_ID, DELEGATION_RECORD_TAG, MAX_DELEGATE_DATA,
};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::MARKET_SEED;
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, base_mint, quote_mint, owner_program, delegate_buffer, delegation_record, delegation_metadata, system_program, delegation_program, ..] =
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
    assert_owned_by(market, pid)?; // owned by us → not yet delegated
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // Re-derive the canonical market PDA from its mints → validates + gives bump.
    let bump = assert_pda(
        market,
        &[MARKET_SEED, &base_mint.key()[..], &quote_mint.key()[..]],
        pid,
    )?;

    // Pin the delegation accounts (derived from the DELEGATED account = market).
    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    let mk = market.key();
    if delegate_buffer.key() != &find_program_address(&[DELEGATE_BUFFER_TAG, &mk[..]], pid).0
        || delegation_record.key()
            != &find_program_address(&[DELEGATION_RECORD_TAG, &mk[..]], &DELEGATION_PROGRAM_ID).0
        || delegation_metadata.key()
            != &find_program_address(&[DELEGATION_METADATA_TAG, &mk[..]], &DELEGATION_PROGRAM_ID).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    let bump_arr = [bump];
    let mut buf = [0u8; MAX_DELEGATE_DATA];
    let len = write_delegate_data(
        &mut buf,
        commit_frequency_ms,
        &[MARKET_SEED, &base_mint.key()[..], &quote_mint.key()[..], &bump_arr[..]],
        if has_validator { Some(&validator) } else { None },
    );
    let seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(&base_mint.key()[..]),
        Seed::from(&quote_mint.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_delegate(
        &DelegateAccounts {
            payer: authority,
            delegated_account: market,
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
