//! delegate_market_book — delegate this market's order-book PDA to the ER so the
//! MagicBlock matcher can run on it. Faithful port of the Anchor
//! `delegate_market_book`. Authority-gated; the book PDA signs the Delegate CPI
//! as itself. Also stamps `book_delegated_at_slot` (the censorship baseline for
//! the permissionless force-undelegate escape).
//!
//! accounts: [authority (signer, payer), market (program-owned, w),
//!            market_book (PDA owned by us, w, signs), owner_program (THIS
//!            program), delegate_buffer, delegation_record, delegation_metadata,
//!            system_program, delegation_program]
//! data: [commit_frequency_ms u32][has_validator u8][validator [u8;32] if has]

use crate::book::MARKET_BOOK_SEED;
use crate::er::{
    cpi_delegate, write_delegate_data, DelegateAccounts, DELEGATE_BUFFER_TAG,
    DELEGATION_METADATA_TAG, DELEGATION_PROGRAM_ID, DELEGATION_RECORD_TAG, MAX_DELEGATE_DATA,
};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, market_book, owner_program, delegate_buffer, delegation_record, delegation_metadata, system_program, delegation_program, ..] =
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

    // ── auth + market binding ───────────────────────────────────────────
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

    // The book PDA must be owned by US (not yet delegated) before we sign over it.
    let bump = assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], pid)?;
    if !market_book.is_owned_by(pid) {
        return Err(ProgramError::Custom(210)); // already delegated / wrong owner
    }

    // Pin the delegation accounts: owner_program is us; buffer under us; record +
    // metadata under the delegation program.
    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    let mb = market_book.key();
    if delegate_buffer.key() != &find_program_address(&[DELEGATE_BUFFER_TAG, &mb[..]], pid).0
        || delegation_record.key()
            != &find_program_address(&[DELEGATION_RECORD_TAG, &mb[..]], &DELEGATION_PROGRAM_ID).0
        || delegation_metadata.key()
            != &find_program_address(&[DELEGATION_METADATA_TAG, &mb[..]], &DELEGATION_PROGRAM_ID).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    // ── build the Delegate args (seeds re-derive the book PDA) + CPI ────
    let bump_arr = [bump];
    let mut buf = [0u8; MAX_DELEGATE_DATA];
    let len = write_delegate_data(
        &mut buf,
        commit_frequency_ms,
        // Re-audit 2026-06-30 (HIGH): args.seeds must NOT include the bump. The DLP
        // echoes them to the undelegate callback, which re-derives the PDA via
        // find_program_address (adding the bump itself); a bump here yields a wrong
        // address → the account can NEVER be undelegated (state/funds trapped on ER).
        // The bump travels only in the invoke_signed Signer seeds below. Matches the
        // anchor 2026-06-28 fix; pin's process_external_undelegate expects book=2 seeds.
        &[MARKET_BOOK_SEED, &market.key()[..]],
        if has_validator { Some(&validator) } else { None },
    );
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_delegate(
        &DelegateAccounts {
            payer: authority,
            delegated_account: market_book,
            owner_program,
            delegate_buffer,
            delegation_record,
            delegation_metadata,
            system_program,
            delegation_program,
        },
        &buf[..len],
        &signer,
    )?;

    // Censorship baseline: even if the sequencer never posts a fill, the
    // force-undelegate timeout starts ticking from here.
    let slot = Clock::get()?.slot;
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.book_delegated_at_slot = slot;
    }
    Ok(())
}
