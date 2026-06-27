//! verify_market_invariants — permissionless market-health check with a
//! self-protective auto-halt. Asserts the open-interest invariant
//! (`long_oi_lots == short_oi_lots`, every long lot matched by a short). If it
//! ever breaks, the market is flipped to Paused so no new orders land against a
//! corrupt book, and the call reverts so the breach is visible to monitors.
//!
//! Mirrors the anchor `verify_market_invariants` OI invariant (code 5). The
//! anchor S7 ER-stall *liveness* sub-check is intentionally NOT ported here: it
//! needs `last_mark_update_slot` / `last_heartbeat_slot` on the market, which the
//! port has not carved yet (no heartbeat instruction stamps them). It will be
//! added with the ER liveness batch; until then this enforces the core
//! value-conservation invariant.
//!
//! accounts: [market (program-owned, w — may auto-pause)]

use crate::guard::{assert_disc, assert_owned_by};
use crate::state::{Market, MARKET_DISC, MARKET_STATUS_PAUSED};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Custom error code: open-interest imbalance detected (anchor invariant 5).
pub const OPEN_INTEREST_IMBALANCE: u32 = 105;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Genuine market of THIS program (owner + discriminator). No authority gate:
    // the check is permissionless and can only pause-on-breach, never re-open.
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;

    let imbalanced = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        m.long_oi_lots != m.short_oi_lots
    };

    if imbalanced {
        // Auto-halt: flip to Paused (idempotent if already paused). The port has
        // no terminal Closed state yet, so there is nothing to preserve.
        unsafe {
            let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
            m.status = MARKET_STATUS_PAUSED;
        }
        return Err(ProgramError::Custom(OPEN_INTEREST_IMBALANCE));
    }
    Ok(())
}
