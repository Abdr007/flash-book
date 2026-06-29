//! undelegate_market_book — return the order-book PDA from the ER back to this
//! program. Faithful port of the Anchor `undelegate_market_book`. Authority-gated;
//! the book PDA signs the Undelegate CPI as itself. (The permissionless
//! censorship-escape variant `force_undelegate_market_book` is a separate ix.)
//!
//! accounts: [authority (signer, payer), market (program-owned, r),
//!            market_book (delegated PDA, w, signs), owner_program (THIS program),
//!            delegate_buffer, system_program, delegation_program]
//! data: (none)

use crate::book::MARKET_BOOK_SEED;
use crate::er::{cpi_undelegate, UndelegateAccounts, DELEGATE_BUFFER_TAG};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, market_book, owner_program, delegate_buffer, system_program, delegation_program, ..] =
        accounts
    else {
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

    // Bind the book PDA by seed (it is currently delegated → owned by the
    // delegation program, so we don't check our ownership here) and the buffer.
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
            payer: authority,
            delegated_account: market_book,
            owner_program,
            buffer: delegate_buffer,
            system_program,
            delegation_program,
        },
        &signer,
    )
}
