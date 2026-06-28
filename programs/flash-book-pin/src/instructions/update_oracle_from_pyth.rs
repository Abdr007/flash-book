//! update_oracle_from_pyth — PERMISSIONLESS mark update from a verified Pyth
//! `PriceUpdateV2` account. Faithful port of the Anchor `update_oracle_from_pyth`,
//! adapted to pin's mark-folded model (the converted price becomes the mark).
//!
//! Unlike the trusted sequencer path (`update_oracle`), this is permissionless:
//! the price is cryptographically verified by Pyth's receiver program, so any
//! keeper may crank it. Safety comes from (1) `price_update.owner ==
//! PYTH_RECEIVER_PROGRAM_ID`, (2) the discriminator + FULL-verification +
//! feed-id + freshness checks in the byte parser, (3) the per-market confidence
//! cap, and (4) the OPTIONAL envelope rate-limit (as in update_oracle).
//!
//! accounts: [caller (signer), market (program-owned, w),
//!            oracle_config (program-owned [b"oracle_config", market], r),
//!            price_update (Pyth-receiver-owned, r),
//!            envelope_config (PDA, owned, r) — OPTIONAL]
//! data: (none)

use crate::constants::BPS_DENOM;
use crate::envelope::gate_price_move;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::pyth_oracle::{get_price_no_older_than_full, pyth_price_to_ticks, PYTH_RECEIVER_PROGRAM_ID};
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{
    Market, MarketEnvelopeConfig, MarketOracleConfig, ENVELOPE_CONFIG_DISC, MARKET_DISC,
    ORACLE_CONFIG_DISC, ORACLE_SOURCE_PYTH,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, oracle_config, price_update, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;

    let now_slot = Clock::get()?.slot;
    let now_unix = Clock::get()?.unix_timestamp;
    let (old_mark, last_slot) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.mark_price_ticks, m.last_mark_update_slot)
    };

    // Oracle config must select Pyth + carry the feed id / freshness / conf caps.
    assert_owned_by(oracle_config, program_id)?;
    assert_disc(oracle_config, &ORACLE_CONFIG_DISC)?;
    let (feed_id, max_staleness, max_confidence, tick_decimals) = {
        let d = oracle_config.try_borrow_data()?;
        let c = unsafe { &*(d.as_ptr() as *const MarketOracleConfig) };
        if &c.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if c.source != ORACLE_SOURCE_PYTH {
            return Err(ProgramError::InvalidArgument);
        }
        (c.pyth_price_feed_id, c.max_staleness_seconds, c.max_confidence_bps, c.tick_decimals)
    };

    // The price account MUST be owned by the Pyth receiver (its verification gives
    // the price its integrity — `Account<PriceUpdateV2>` checked this for free).
    if !price_update.is_owned_by(&PYTH_RECEIVER_PROGRAM_ID) {
        return Err(ProgramError::IllegalOwner);
    }
    let price = {
        let d = price_update.try_borrow_data()?;
        get_price_no_older_than_full(&d, &feed_id, now_unix, max_staleness as u64)
            .map_err(|_| ProgramError::Custom(190))? // stale / mismatch / bad data
    };
    if price.price <= 0 {
        return Err(ProgramError::InvalidArgument);
    }

    let new_ticks =
        pyth_price_to_ticks(price.price, price.exponent, tick_decimals).ok_or(ProgramError::InvalidArgument)?;

    // Confidence in bps of price; reject if wider than the cap (0 = gate off).
    if max_confidence > 0 {
        let conf_bps = (price.conf as u128)
            .saturating_mul(BPS_DENOM as u128)
            / (price.price as u128); // price > 0 checked
        if conf_bps > max_confidence as u128 {
            return Err(ProgramError::Custom(191)); // confidence too wide
        }
    }

    // ── OPTIONAL envelope gate on the new price (as in update_oracle) ───
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
        gate_price_move(old_mark, new_ticks, dt_slots, cap).map_err(|_| ProgramError::Custom(123))?;
    }

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.mark_price_ticks = new_ticks;
        if now_slot > m.last_mark_update_slot {
            m.last_mark_update_slot = now_slot;
        }
    }
    Ok(())
}
