//! expand_market_book — grow a market's order-book account by `additional_nodes`
//! (each `NODE_TOTAL_BYTES`), so the hypertree arena can hold more resting
//! orders/seats. Market-authority gated. Tops up the rent for the larger size,
//! then `resize`s (which zero-extends the appended tail, giving the free-list /
//! bump allocator clean memory). Capped at `MARKET_BOOK_MAX_TOTAL_BYTES`.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            market_book (PDA, owned, w), system_program]
//! data: additional_nodes (u32 LE)

use crate::book::{
    MarketBookHandle, MARKET_BOOK_DISC, MARKET_BOOK_MAX_TOTAL_BYTES, MARKET_BOOK_SEED,
    NODE_TOTAL_BYTES,
};
use crate::cpi::system_transfer;
use crate::guard::{assert_market, assert_owned_by, assert_pda, assert_signer};
use crate::state::Market;
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

/// Max bytes an account's data may grow per instruction (Solana runtime).
const MAX_PERMITTED_DATA_INCREASE: usize = 10_240;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, market_book, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let additional_nodes = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if additional_nodes == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── auth: market authority, book bound to market ────────────────────
    assert_signer(authority)?;
    assert_market(market, program_id)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }
    assert_owned_by(market_book, program_id)?;
    assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], program_id)?;
    {
        let d = market_book.try_borrow_data()?;
        if d.len() < 8 || d[..8] != MARKET_BOOK_DISC {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // ── size math (node-aligned, bounded) ───────────────────────────────
    let additional_bytes = additional_nodes
        .checked_mul(NODE_TOTAL_BYTES)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if additional_bytes > MAX_PERMITTED_DATA_INCREASE {
        return Err(ProgramError::InvalidArgument);
    }
    let old_len = market_book.data_len();
    let new_len = old_len
        .checked_add(additional_bytes)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if new_len > MARKET_BOOK_MAX_TOTAL_BYTES {
        return Err(ProgramError::InvalidArgument);
    }

    // ── top up rent for the larger account, then grow ───────────────────
    let new_minimum = Rent::get()?.minimum_balance(new_len);
    let cur_lamports = market_book.lamports();
    if new_minimum > cur_lamports {
        system_transfer(system_program, authority, market_book, new_minimum - cur_lamports)?;
    }
    market_book.realloc(new_len, true)?; // zero_init = true → clean appended tail

    // ── sanity: the grown account must still parse as a valid book ──────
    {
        let mut d = market_book.try_borrow_mut_data()?;
        MarketBookHandle::from_account_data(&mut d).map_err(|_| ProgramError::InvalidAccountData)?;
    }
    Ok(())
}
