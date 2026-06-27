//! verify_market_invariants — permissionless market-health check with a
//! self-protective auto-halt. Two invariants, each of which pauses the market
//! and reverts on breach so no new orders land against an unsafe book:
//!
//!  * S7 (code 7) — ER-stall liveness: if the market shows no liveness signal
//!    (no fill AND no ER heartbeat) for more than `MARK_STALENESS_MAX_SLOTS`,
//!    the ER is presumed stalled and the market auto-pauses. Liveness is
//!    `max(last_mark_update_slot, last_heartbeat_slot)` — a healthy-but-quiet
//!    market keeps `last_heartbeat_slot` fresh via `er_heartbeat`, so only a
//!    genuinely dead ER trips it. A market that has never stamped either field
//!    (both 0) is not paused on missing data.
//!  * OI (code 5) — open-interest conservation: `long_oi_lots == short_oi_lots`,
//!    every long lot matched by a short.
//!
//! Mirrors the anchor `verify_market_invariants`.
//!
//! accounts: [market (program-owned, w — may auto-pause)]

use crate::constants::MARK_STALENESS_MAX_SLOTS;
use crate::guard::{assert_disc, assert_owned_by};
use crate::state::{Market, MARKET_DISC, MARKET_STATUS_PAUSED};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

/// Custom error code: open-interest imbalance detected (anchor invariant 5).
pub const OPEN_INTEREST_IMBALANCE: u32 = 105;
/// Custom error code: ER-stall liveness breach (anchor invariant 7).
pub const MARKET_LIVENESS_STALL: u32 = 107;

fn pause(market: &AccountInfo) {
    // Auto-halt: flip to Paused (idempotent if already paused). The port has no
    // terminal Closed state yet, so there is nothing to preserve.
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.status = MARKET_STATUS_PAUSED;
    }
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Genuine market of THIS program (owner + discriminator). No authority gate:
    // the check is permissionless and can only pause-on-breach, never re-open.
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;

    let (liveness_slot, imbalanced) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (
            m.last_mark_update_slot.max(m.last_heartbeat_slot),
            m.long_oi_lots != m.short_oi_lots,
        )
    };

    // S7 — ER-stall liveness. Guard on `> 0` so a market that has never stamped
    // either field is not paused on missing data.
    if liveness_slot > 0 {
        let current_slot = Clock::get()?.slot;
        if current_slot.saturating_sub(liveness_slot) > MARK_STALENESS_MAX_SLOTS {
            pause(market);
            return Err(ProgramError::Custom(MARKET_LIVENESS_STALL));
        }
    }

    if imbalanced {
        pause(market);
        return Err(ProgramError::Custom(OPEN_INTEREST_IMBALANCE));
    }
    Ok(())
}
