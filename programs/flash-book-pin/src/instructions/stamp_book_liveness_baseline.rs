//! stamp_book_liveness_baseline — start the censorship/liveness clock for a book
//! that has been DELEGATED to the ER. One-shot: records the slot at which the
//! book became delegated (`market.book_delegated_at_slot`), the baseline a later
//! force-undelegate / escape path measures censorship against. Faithful port of
//! the Anchor `stamp_book_liveness_baseline`.
//!
//! Permissionless: anyone may start the clock once the book is actually
//! delegated. No CPI — it only reads the book's owner and stamps the market.
//!
//! accounts: [payer (signer), market (program-owned, w),
//!            market_book (delegated PDA, r)]
//! data: (none)

use crate::book::MARKET_BOOK_SEED;
use crate::er::DELEGATION_PROGRAM_ID;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, market, market_book, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(payer)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;

    // The book is the canonical PDA but currently DELEGATED, so it is owned by the
    // delegation program (NOT us) — bind it by seed re-derivation, then require
    // delegated ownership. (assert_market_book can't be used; it checks OUR
    // ownership, which a delegated book fails.)
    assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], pid)?;
    if !market_book.is_owned_by(&DELEGATION_PROGRAM_ID) {
        return Err(ProgramError::Custom(200)); // book not delegated → no clock to start
    }

    // One-shot: refuse if a baseline is already stamped.
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if m.book_delegated_at_slot != 0 {
            return Err(ProgramError::Custom(201)); // already stamped
        }
    }

    let slot = Clock::get()?.slot;
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.book_delegated_at_slot = slot;
    }
    Ok(())
}
