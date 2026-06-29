//! update_oracle — set the market's mark price. Sequencer-gated (the same key
//! that authorizes settlement is the price authority here, mirroring
//! `apply_fill`). A dedicated oracle authority / quorum / Pyth pull are separate
//! anchor instructions that can be ported as refinements.
//!
//! Defense in depth: an OPTIONAL `envelope_config` account (mirroring anchor
//! `update_oracle`'s optional envelope gate) bounds the per-update mark move to
//! the market's configured `max_price_move_bps_per_slot × elapsed_slots`. When it
//! is supplied the move is rate-limited.
//!
//! HONEST LIMITATION (re-audit 2026-06): the envelope is OPTIONAL and the
//! sequencer builds the transaction, so a compromised sequencer simply omits the
//! account to bypass the cap — this gate constrains a BUGGY sequencer, NOT a
//! compromised one. In pin's model the sequencer IS the trusted price authority
//! (it sets `mark_price_ticks` directly here, where anchor stages an
//! authority-gated oracle bridged by a separate rate-limited `settle_mark`). A
//! mandatory-envelope arming flag is a deferred hardening — see docs/PIN_AUDIT.
//!
//! Setting the mark is also a mark-freshness event: it stamps
//! `last_mark_update_slot`, the mark half of the ER-liveness signal that
//! `verify_market_invariants` reads (the heartbeat half is `er_heartbeat`). So a
//! market the sequencer keeps priced stays Active without needing a separate
//! heartbeat.
//!
//! accounts: [sequencer (signer), market (PDA, owned, w),
//!            envelope_config (PDA, owned, r) — OPTIONAL]
//! data: mark_price_ticks (u64 LE, must be > 0)

use crate::envelope::gate_price_move;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{Market, MarketEnvelopeConfig, ENVELOPE_CONFIG_DISC, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [sequencer, market, rest @ ..] = accounts else {
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

    // Snapshot the current mark + last-update slot under a shared borrow before
    // gating / mutating. Authorize the sequencer at the same time.
    let now_slot = Clock::get()?.slot;
    let (old_mark, last_slot) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.sequencer != sequencer.key() {
            return Err(ProgramError::IllegalOwner);
        }
        (m.mark_price_ticks, m.last_mark_update_slot)
    };

    // ── OPTIONAL envelope gate (defense-in-depth rate limit) ────────────
    // If an envelope_config is supplied it MUST be this market's canonical PDA;
    // a breach reverts before any mutation. dt = slots since the last mark
    // update (gate_price_move admits any move when old_mark == 0, i.e. first).
    if let [envelope_config, ..] = rest {
        assert_owned_by(envelope_config, program_id)?;
        assert_pda(envelope_config, &[ENVELOPE_CONFIG_SEED, &market.key()[..]], program_id)?;
        assert_disc(envelope_config, &ENVELOPE_CONFIG_DISC)?;
        let cap = {
            let d = envelope_config.try_borrow_data()?;
            let c = unsafe { &*(d.as_ptr() as *const MarketEnvelopeConfig) };
            if &c.market != market.key() {
                return Err(ProgramError::InvalidArgument);
            }
            c.max_price_move_bps_per_slot
        };
        let dt_slots = now_slot.saturating_sub(last_slot);
        gate_price_move(old_mark, mark_price_ticks, dt_slots, cap)
            .map_err(|_| ProgramError::Custom(123))?;
    }

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
