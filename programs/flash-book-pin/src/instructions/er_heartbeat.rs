//! er_heartbeat — the market's settlement signer (the ER sequencer) attests the
//! ephemeral rollup is live by stamping `last_heartbeat_slot`. A live-but-quiet
//! market (no fills) keeps this fresh so `verify_market_invariants` does not
//! auto-pause it; only a market with no fill AND no heartbeat for
//! `MARK_STALENESS_MAX_SLOTS` is presumed stalled.
//!
//! Auth (anchor C-1 trust model): ONLY `market.sequencer` may call. A zero/unset
//! sequencer (legacy market) fails closed. The slot is monotonic — a stale or
//! replayed heartbeat can never move the signal backward.
//!
//! accounts: [sequencer (signer), market (program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [sequencer, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(sequencer)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;

    let slot = Clock::get()?.slot;
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        // Fail closed on an unset sequencer; only the configured signer attests.
        if m.sequencer != *sequencer.key() {
            return Err(ProgramError::MissingRequiredSignature);
        }
        // Monotonic: never move the liveness signal backward on a replay.
        if slot < m.last_heartbeat_slot {
            return Err(ProgramError::InvalidArgument);
        }
        m.last_heartbeat_slot = slot;
    }
    Ok(())
}
