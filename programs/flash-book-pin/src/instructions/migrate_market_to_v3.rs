//! migrate_market_to_v3 — authority-gated. In Anchor this reallocs a PRE-V3
//! Market to the v3 size and backfills v3-default params. On flash-book-pin there
//! is no pre-v3 layout: every Market is created in the final canonical layout by
//! `initialize_market` (the 1152-byte `Market` pod). So the only reachable state
//! on pin is the Anchor "already migrated" branch — this instruction verifies the
//! account IS canonical (program-owned, right discriminator, correct size, caller
//! is the market authority) and is a no-op. Ported for parity / forward-compat.
//!
//! accounts: [authority (signer), market (program-owned)]
//! data: (none)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_signer(authority)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    // Canonical size + authority — confirms the account is already in the final
    // (only) pin layout; nothing to migrate.
    if market.data_len() != core::mem::size_of::<Market>() {
        return Err(ProgramError::InvalidAccountData);
    }
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }
    Ok(()) // already canonical (pin Markets are born v3)
}
