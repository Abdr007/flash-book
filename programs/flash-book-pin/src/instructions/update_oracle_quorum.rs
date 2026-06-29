//! update_oracle_quorum — set the market's mark from a 3-source oracle quorum.
//! Faithful port of the Anchor `update_oracle_quorum`, adapted to pin's
//! mark-folded model: the accepted MEDIAN becomes `mark_price_ticks` (pin has no
//! separate oracle_price_ticks; the sequencer is the unified price authority, as
//! in `update_oracle`).
//!
//! Each source is gated on staleness + confidence and the set on dispersion via
//! the host-tested `oracle_quorum::aggregate_median`, using the per-market
//! `MarketOracleConfig` limits. The accepted median then passes the SAME optional
//! envelope rate-limit + mark-freshness stamp as `update_oracle`.
//!
//! accounts: [sequencer (signer), market (program-owned, w),
//!            oracle_config (program-owned [b"oracle_config", market], r),
//!            envelope_config (PDA, owned, r) — OPTIONAL]
//! data: [prices [u64;3]][confidences [u64;3]][published_at [u64;3]] — 72 bytes

use crate::envelope::gate_price_move;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::oracle_quorum::aggregate_median;
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{
    Market, MarketEnvelopeConfig, MarketOracleConfig, ENVELOPE_CONFIG_DISC, MARKET_DISC,
    ORACLE_CONFIG_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

fn read_3(data: &[u8], base: usize) -> [u64; 3] {
    [
        u64::from_le_bytes(data[base..base + 8].try_into().unwrap()),
        u64::from_le_bytes(data[base + 8..base + 16].try_into().unwrap()),
        u64::from_le_bytes(data[base + 16..base + 24].try_into().unwrap()),
    ]
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [sequencer, market, oracle_config, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 72 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let prices = read_3(data, 0);
    let confidences = read_3(data, 24);
    let published_at = read_3(data, 48);

    assert_signer(sequencer)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;

    let now_slot = Clock::get()?.slot;
    let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;

    // Authorize the sequencer + snapshot mark/last-slot.
    let (old_mark, last_slot) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.sequencer != sequencer.key() {
            return Err(ProgramError::IllegalOwner);
        }
        (m.mark_price_ticks, m.last_mark_update_slot)
    };

    // Per-market quorum limits.
    assert_owned_by(oracle_config, program_id)?;
    assert_disc(oracle_config, &ORACLE_CONFIG_DISC)?;
    let (max_staleness, max_confidence, max_dispersion) = {
        let d = oracle_config.try_borrow_data()?;
        let c = unsafe { &*(d.as_ptr() as *const MarketOracleConfig) };
        if &c.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        (c.max_staleness_seconds, c.max_confidence_bps, c.max_dispersion_bps)
    };

    let median = aggregate_median(
        prices, confidences, published_at, now_unix,
        max_staleness, max_confidence, max_dispersion,
    )
    .map_err(|_| ProgramError::InvalidArgument)?;

    // ── OPTIONAL envelope gate on the accepted median (as in update_oracle) ─
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
        gate_price_move(old_mark, median, dt_slots, cap).map_err(|_| ProgramError::Custom(123))?;
    }

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.mark_price_ticks = median;
        if now_slot > m.last_mark_update_slot {
            m.last_mark_update_slot = now_slot;
        }
    }
    Ok(())
}
