//! init_market_book — create a market's order-book account (the hypertree-backed
//! bid/ask/seat arena), PDA `[b"market_book", market]`, and format its header.
//! The matching FOUNDATION: order placement / matching build on this account.
//! Market-authority gated. NO funds, NO matching yet — creates an EMPTY book.
//!
//! The base/quote mints are passed and PROVEN to be the market's (the market PDA
//! is `[b"market", base_mint, quote_mint]`), then stored in the book header.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            base_mint (r), quote_mint (r), market_book (PDA, w, uninit),
//!            system_program]

use crate::book::{MarketBookHandle, MARKET_BOOK_SEED, MARKET_BOOK_TOTAL_BYTES};
use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::MARKET_SEED;
use crate::state::Market;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, base_mint, quote_mint, market_book, system_program, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── auth: market authority, mints proven to be the market's ─────────
    assert_signer(authority)?;
    assert_market(market, program_id)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }
    // Prove (base_mint, quote_mint) ARE this market's mints: re-derive the market
    // PDA from them and require it equals the passed market account.
    assert_pda(
        market,
        &[MARKET_SEED, &base_mint.key()[..], &quote_mint.key()[..]],
        program_id,
    )?;

    // ── create the book PDA ─────────────────────────────────────────────
    assert_uninitialized(market_book)?;
    let bump = assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], program_id)?;
    let lamports = Rent::get()?.minimum_balance(MARKET_BOOK_TOTAL_BYTES);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        market_book,
        system_program,
        lamports,
        MARKET_BOOK_TOTAL_BYTES as u64,
        program_id,
        &signer,
    )?;

    // ── stamp the disc + format the header (NIL roots, empty free list) ──
    let market_key = *market.key();
    let base = *base_mint.key();
    let quote = *quote_mint.key();
    unsafe {
        let data = market_book.borrow_mut_data_unchecked();
        MarketBookHandle::write_disc_and_init_header(data, bump, market_key, base, quote)
            .map_err(|_| ProgramError::InvalidAccountData)?;
    }
    Ok(())
}
