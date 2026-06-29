//! vault_cancel_order_v3 — the strategist cancels a vault-PDA order, removing it
//! from the book. Faithful port of the Anchor `vault_cancel_order_v3`. Same book
//! lookup/remove path as `cancel_order`, but the order's owner is the VAULT PDA
//! and the authorizer is the vault's strategist.
//!
//! accounts: [strategist (signer), vault (program-owned, r), market
//!            (program-owned, r), market_book (PDA, w)]
//! data: [side u8][order_id u64]   — 9 bytes

use crate::book::MarketBookHandle;
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::hypertree::NIL;
use crate::state::{VaultV3, VAULT_V3_DISC};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [strategist, vault, market, market_book, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let side = data[0];
    let order_id = u64::from_le_bytes(data[1..9].try_into().unwrap());

    assert_signer(strategist)?;
    if side > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Only the vault's strategist may cancel the vault PDA's orders.
    assert_owned_by(vault, pid)?;
    assert_disc(vault, &VAULT_V3_DISC)?;
    let vault_pk = *vault.key();
    {
        let d = vault.try_borrow_data()?;
        let v = unsafe { &*(d.as_ptr() as *const VaultV3) };
        if &v.strategist != strategist.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;

    unsafe {
        let book_data = market_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        if &handle.header.market_pubkey != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let side_is_bid = side == 0;
        let idx = if side_is_bid {
            handle.lookup_bid_by_order_id(order_id)
        } else {
            handle.lookup_ask_by_order_id(order_id)
        };
        if idx == NIL {
            return Err(ProgramError::Custom(4)); // not found
        }
        // The order must belong to THIS vault.
        if handle.order_at(idx).trader != vault_pk {
            return Err(ProgramError::Custom(1100)); // wrong trader
        }
        // Re-audit 2026-06 (MED): a forced-liquidation order (type 3) carries the
        // vault's trader when the vault's own position is liquidated; the strategist
        // must not be able to cancel it (liquidation evasion). Mirror cancel_order's
        // #187 type-3 guard on this vault-order path.
        if handle.order_at(idx).order_type == 3 {
            return Err(ProgramError::Custom(1101)); // cannot cancel forced-liquidation
        }
        if side_is_bid {
            handle.remove_bid_node(idx);
        } else {
            handle.remove_ask_node(idx);
        }
    }
    Ok(())
}
