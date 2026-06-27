//! update_oracle — set the market's mark price. Sequencer-gated (the same key
//! that authorizes settlement is the price authority here, mirroring
//! `apply_fill`). A dedicated oracle authority / quorum / Pyth pull are separate
//! anchor instructions that can be ported as refinements.
//!
//! Setting the mark is also a mark-freshness event: it stamps
//! `last_mark_update_slot`, the mark half of the ER-liveness signal that
//! `verify_market_invariants` reads (the heartbeat half is `er_heartbeat`). So a
//! market the sequencer keeps priced stays Active without needing a separate
//! heartbeat.
//!
//! accounts: [sequencer (signer), market (PDA, owned, w)]
//! data: mark_price_ticks (u64 LE, must be > 0)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [sequencer, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mark_price_ticks = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if mark_price_ticks == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(sequencer)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.sequencer != sequencer.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    let now_slot = Clock::get()?.slot;
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.mark_price_ticks = mark_price_ticks;
        // Mark-freshness stamp (liveness signal). Monotonic: never regress on a
        // re-ordered call.
        if now_slot > m.last_mark_update_slot {
            m.last_mark_update_slot = now_slot;
        }
    }
    Ok(())
}
