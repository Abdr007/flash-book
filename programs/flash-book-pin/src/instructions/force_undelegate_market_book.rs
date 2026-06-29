//! force_undelegate_market_book — PERMISSIONLESS censorship / ER-stall escape.
//! Faithful port of the Anchor `force_undelegate_market_book`. `undelegate_market_book`
//! is authority-gated, so a censoring sequencer or a dead ER can trap traders
//! (they can't post a closing order while delegated, and withdraw needs flat).
//! This variant drops the authority check and gates ONLY on settlement liveness:
//! the book must have been silent past the timeout (`er::force_undelegate_allowed`,
//! Kani-proven to never fire while the ER is live). The Undelegate CPI is signed
//! by the program via the book PDA — no sequencer signature.
//!
//! accounts: [payer (signer), market (program-owned, r), market_book (delegated
//!            PDA, w, signs), owner_program (THIS program), delegate_buffer,
//!            system_program, delegation_program]
//! data: (none)

use crate::book::MARKET_BOOK_SEED;
use crate::constants::{CENSORSHIP_ESCAPE_TIMEOUT_SLOTS, FORCE_UNDELEGATE_TIMEOUT_SLOTS};
use crate::er::{cpi_undelegate, force_undelegate_allowed, UndelegateAccounts, DELEGATE_BUFFER_TAG};
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

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, market, market_book, owner_program, delegate_buffer, system_program, delegation_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(payer)?;
    // The market itself stays on L1 (only the book is delegated), so it's ours.
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;

    let current_slot = Clock::get()?.slot;
    let (last_fill, heartbeat, delegated_at) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.last_mark_update_slot, m.last_heartbeat_slot, m.book_delegated_at_slot)
    };

    // Permissionless gate: opens ONLY when the ER is stalled / censoring.
    if !force_undelegate_allowed(
        current_slot,
        last_fill,
        heartbeat,
        delegated_at,
        FORCE_UNDELEGATE_TIMEOUT_SLOTS,
        CENSORSHIP_ESCAPE_TIMEOUT_SLOTS,
    ) {
        return Err(ProgramError::Custom(220)); // ER still live
    }

    // Bind the delegated book PDA (owned by the delegation program) + buffer.
    let bump = assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], pid)?;
    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    if delegate_buffer.key()
        != &find_program_address(&[DELEGATE_BUFFER_TAG, &market_book.key()[..]], pid).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_undelegate(
        &UndelegateAccounts {
            payer,
            delegated_account: market_book,
            owner_program,
            buffer: delegate_buffer,
            system_program,
            delegation_program,
        },
        &signer,
    )
}
