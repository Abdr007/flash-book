//! advance_funding — permissionless crank that advances the market's funding rate
//! and cumulative funding index from the on-chain OI skew (Wave 25b/37). Funding is
//! SKEW-based (GMX-V2 velocity-smoothed) and MARK-ONLY-friendly: the rate ramps
//! toward a target derived from `long_oi − short_oi` at a bounded velocity, and
//! accrues into `cum_funding_index` (Q64.64) — which `settle_position_funding`
//! already applies to positions on every fill / `settle_funding`. The result is
//! DETERMINISTIC given (OI, dt, rate, config), so the crank is permissionless; the
//! only caller freedom is timing, and the accrual is the average rate × elapsed dt
//! (frequent cranks → MORE accurate, never an advantage).
//!
//! Fail-safe: no-op on a paused or stale-mark market (a dead market accrues no
//! funding); INERT until `set_funding_params` sets a non-zero config (skew_factor 0
//! ⇒ target 0, velocity 0 ⇒ rate pinned, max_rate 0 ⇒ no accrual — pre-field markets
//! are unchanged); per-crank dt is clamped to MAX_FUNDING_DT_SLOTS so a long silence
//! can't produce a huge one-shot index jump; the first call just stamps the baseline.
//!
//! accounts: [caller (signer), market (PDA, w)]
//! data: (none)

use crate::funding_velocity::{funding_index_delta_q64, ramp_rate_e9, target_rate_from_skew_e9};
use crate::guard::{assert_market, assert_signer};
use crate::state::{Market, MARKET_STATUS_ACTIVE};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_signer(caller)?;
    assert_market(market, pid)?;

    let now = Clock::get()?.slot;
    let (status, last_mark_update, last_funding, long_oi, short_oi, current_rate, skew_factor, velocity, max_rate, cum) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (
            m.status, m.last_mark_update_slot, m.last_funding(), m.long_oi_lots, m.short_oi_lots,
            m.funding_rate(), m.funding_skew_factor(), m.funding_velocity(), m.max_funding_rate(),
            m.cum_funding(),
        )
    };

    // Accrue only on a live (active, fresh-marked) market.
    if status != MARKET_STATUS_ACTIVE {
        return Ok(()); // paused → no-op
    }
    if now.saturating_sub(last_mark_update) > crate::constants::MARK_STALENESS_MAX_SLOTS {
        return Ok(()); // stale mark → a dead market accrues no funding
    }

    // First call (or a just-enabled market with no baseline) → stamp, no accrual.
    if last_funding == 0 {
        unsafe {
            (*(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market)).set_last_funding(now);
        }
        return Ok(());
    }
    let dt_raw = now.saturating_sub(last_funding);
    if dt_raw == 0 {
        return Ok(()); // same slot → no-op
    }
    // Bound the per-crank accrual (a long silence accrues at most this, then resumes).
    let dt = dt_raw.min(crate::constants::MAX_FUNDING_DT_SLOTS);

    // Ramp the rate toward the OI-skew target; accrue the trapezoidal-average rate.
    let skew = (long_oi as i128 - short_oi as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    let denom = long_oi.saturating_add(short_oi);
    let target = target_rate_from_skew_e9(skew, denom, skew_factor, max_rate);
    let new_rate = ramp_rate_e9(current_rate, target, velocity, dt, max_rate);
    let avg_rate = ((current_rate as i128 + new_rate as i128) / 2) as i64;
    let index_delta =
        funding_index_delta_q64(avg_rate, dt).ok_or(ProgramError::ArithmeticOverflow)?;
    let new_cum = cum.checked_add(index_delta).ok_or(ProgramError::ArithmeticOverflow)?;

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.set_cum_funding(new_cum);
        m.set_funding_rate(new_rate);
        m.set_last_funding(now);
    }
    Ok(())
}
