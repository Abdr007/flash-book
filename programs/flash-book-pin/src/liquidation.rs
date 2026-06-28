//! Liquidation settlement math — the pure shortfall/penalty computation the
//! `liquidate_position_v2` instruction calls. De-anchored port of
//! `matcher/liquidation.rs::compute_shortfall` (verbatim arithmetic).
//!
//! The `Vec`-based batch helpers (`detect_liquidations`,
//! `generate_liquidation_orders`) are keeper-side and need `no_std` buffers —
//! deferred. This is the per-position settlement core.

use crate::error::{OrOverflow, Result};
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{MarketSnapshot, PositionSnapshot};

const BPS_DENOM: u128 = 10_000;

/// Bankruptcy-resolution result for a single liquidation fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortfallResult {
    pub liquidation_penalty_quote_lots: u64,
    pub shortfall_quote_lots: u64,
    pub collateral_recovered_quote_lots: u64,
}

/// Realized shortfall for a position closed at `fill_price`. `remaining =
/// collateral + pnl − penalty`: ≥0 → that much is recovered (no shortfall);
/// <0 → the deficit is the insurance-fund shortfall. i128→u64 saturates by
/// design (every step is `checked_*`; saturation is the safe failure mode —
/// never abort a liquidation on an implausibly large notional).
pub fn compute_shortfall(
    pos: &PositionSnapshot,
    fill_price: Ticks,
    collateral_quote_lots: u64,
    market_snapshot: &MarketSnapshot,
    liq_penalty_bps: u32,
) -> Result<ShortfallResult> {
    let sign: i128 = if pos.side == Side::Long { 1 } else { -1 };
    let price_diff = (fill_price.0 as i128)
        .checked_sub(pos.entry_price.0 as i128)
        .or_underflow()?;
    let pnl = (pos.size_lots as i128)
        .checked_mul(price_diff)
        .or_overflow()?
        .checked_mul(market_snapshot.tick_size as i128)
        .or_overflow()?
        .checked_mul(sign)
        .or_overflow()?;
    let penalty = (pos.size_lots as i128)
        .checked_mul(fill_price.0 as i128)
        .or_overflow()?
        .checked_mul(market_snapshot.tick_size as i128)
        .or_overflow()?
        .checked_mul(liq_penalty_bps as i128)
        .or_overflow()?
        .checked_div(BPS_DENOM as i128)
        .or_div_zero()?;
    let remaining = (collateral_quote_lots as i128)
        .checked_add(pnl)
        .or_overflow()?
        .checked_sub(penalty)
        .or_underflow()?;
    let penalty_u64 = if penalty < 0 {
        0
    } else if penalty > u64::MAX as i128 {
        u64::MAX
    } else {
        penalty as u64
    };
    if remaining >= 0 {
        let recovered = if remaining > u64::MAX as i128 { u64::MAX } else { remaining as u64 };
        Ok(ShortfallResult {
            liquidation_penalty_quote_lots: penalty_u64,
            shortfall_quote_lots: 0,
            collateral_recovered_quote_lots: recovered,
        })
    } else {
        let shortfall_signed = -remaining;
        let shortfall = if shortfall_signed > u64::MAX as i128 { u64::MAX } else { shortfall_signed as u64 };
        Ok(ShortfallResult {
            liquidation_penalty_quote_lots: penalty_u64,
            shortfall_quote_lots: shortfall,
            collateral_recovered_quote_lots: 0,
        })
    }
}

/// Insurance-fund payout for a liquidation shortfall (bad debt): pay up to
/// `shortfall` from `balance`, returning `(covered, remaining)`. `covered =
/// min(shortfall, balance)`; `remaining > 0` ⇒ the fund is EXHAUSTED and the
/// rest must be socialized via ADL (`auto_deleverage`). Pure transcription of
/// `matcher/insurance.rs::cover_shortfall` (the caller applies `balance −=
/// covered` and `total_payouts += covered`).
#[inline]
pub fn cover_shortfall(balance: u64, shortfall: u64) -> (u64, u64) {
    let covered = shortfall.min(balance);
    (covered, shortfall - covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_shortfall_pays_up_to_balance() {
        // Fund covers it fully when balance ≥ shortfall.
        assert_eq!(cover_shortfall(1_000, 300), (300, 0));
        // Fund exhausted: pays its whole balance, the rest is the ADL remainder.
        assert_eq!(cover_shortfall(200, 500), (200, 300));
        // Nothing owed, or an empty fund.
        assert_eq!(cover_shortfall(1_000, 0), (0, 0));
        assert_eq!(cover_shortfall(0, 500), (0, 500));
    }

    const MKT: crate::state::Pubkey = [9u8; 32];

    fn pos(side: Side, size: u64, entry: u64) -> PositionSnapshot {
        PositionSnapshot {
            market: MKT,
            side,
            size_lots: size,
            entry_price: Ticks(entry),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0,
        }
    }
    fn mkt() -> MarketSnapshot {
        MarketSnapshot {
            market: MKT,
            mark_price: Ticks(100),
            cum_funding_index: 0,
            maintenance_margin_bps: 100,
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        }
    }

    #[test]
    fn long_in_profit_recovers_with_no_shortfall() {
        // long 10 @100, fill 120, collat 50, penalty 100bps(1%).
        // pnl=(120-100)*10=200; penalty=10*120*100/10000=12; remaining=50+200-12=238.
        let r = compute_shortfall(&pos(Side::Long, 10, 100), Ticks(120), 50, &mkt(), 100).unwrap();
        assert_eq!(r, ShortfallResult {
            liquidation_penalty_quote_lots: 12,
            shortfall_quote_lots: 0,
            collateral_recovered_quote_lots: 238,
        });
    }

    #[test]
    fn long_underwater_produces_shortfall() {
        // long 10 @100, fill 50, collat 50, penalty 100bps.
        // pnl=(50-100)*10=-500; penalty=10*50*100/10000=5; remaining=50-500-5=-455.
        let r = compute_shortfall(&pos(Side::Long, 10, 100), Ticks(50), 50, &mkt(), 100).unwrap();
        assert_eq!(r, ShortfallResult {
            liquidation_penalty_quote_lots: 5,
            shortfall_quote_lots: 455,
            collateral_recovered_quote_lots: 0,
        });
    }

    #[test]
    fn short_in_profit_recovers() {
        // short 10 @100, fill 80, collat 50, penalty 100bps.
        // pnl=(80-100)*10*(-1)=200; penalty=10*80*100/10000=8; remaining=50+200-8=242.
        let r = compute_shortfall(&pos(Side::Short, 10, 100), Ticks(80), 50, &mkt(), 100).unwrap();
        assert_eq!(r, ShortfallResult {
            liquidation_penalty_quote_lots: 8,
            shortfall_quote_lots: 0,
            collateral_recovered_quote_lots: 242,
        });
    }

    #[test]
    fn zero_penalty_bps() {
        let r = compute_shortfall(&pos(Side::Long, 10, 100), Ticks(100), 50, &mkt(), 0).unwrap();
        assert_eq!(r.liquidation_penalty_quote_lots, 0);
        // pnl=0, penalty=0 → remaining=50 recovered.
        assert_eq!(r.collateral_recovered_quote_lots, 50);
        assert_eq!(r.shortfall_quote_lots, 0);
    }

    #[test]
    fn exact_breakeven_is_zero_shortfall() {
        // Construct remaining == 0: collat 5, pnl 0 (fill==entry), penalty 5.
        // penalty=10*100*50/10000=5 (50 bps). remaining=5+0-5=0 → recovered 0, no shortfall.
        let r = compute_shortfall(&pos(Side::Long, 10, 100), Ticks(100), 5, &mkt(), 50).unwrap();
        assert_eq!(r.shortfall_quote_lots, 0);
        assert_eq!(r.collateral_recovered_quote_lots, 0);
        assert_eq!(r.liquidation_penalty_quote_lots, 5);
    }

    /// Bankruptcy-resolution contract fuzz: across 50k random closes, the result
    /// must satisfy `compute_shortfall`'s invariants — recovered and shortfall
    /// are MUTUALLY EXCLUSIVE (a closed position either returns value OR is
    /// bankrupt, never both), a recovery never exceeds the funds at hand
    /// (collateral + penalty headroom), and the call NEVER panics/overflows
    /// (saturation is the designed failure mode). Deterministic LCG.
    #[test]
    fn compute_shortfall_invariants_fuzz() {
        let mut seed: u64 = 0xB16B_00B5_DEAD_BEEF;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        let m = mkt();
        for _ in 0..50_000 {
            let side = if next() % 2 == 0 { Side::Long } else { Side::Short };
            let size = 1 + next() % 1_000;
            let entry = 1 + next() % 10_000;
            let fill = 1 + next() % 10_000;
            let collateral = next() % 1_000_000;
            let penalty_bps = (next() % 2_000) as u32; // 0 ..= ~20%
            let r = compute_shortfall(&pos(side, size, entry), Ticks(fill), collateral, &m, penalty_bps)
                .unwrap();
            // Mutually exclusive: never both recovered AND short.
            assert!(
                !(r.shortfall_quote_lots > 0 && r.collateral_recovered_quote_lots > 0),
                "recovered and shortfall both > 0"
            );
            // A recovery is bounded by collateral + the realized gain, and the
            // gain can never exceed `size · max(entry, fill) · tick` (the
            // largest possible price move, for either side) — a real ceiling
            // that catches any sign/accounting blow-up.
            let max_px = entry.max(fill);
            let gain_ceiling = size.saturating_mul(max_px).saturating_mul(m.tick_size);
            assert!(
                r.collateral_recovered_quote_lots <= collateral.saturating_add(gain_ceiling),
                "recovery exceeds collateral + max-gain ceiling"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Worse-of-(mark, oracle) health price — the CONSERVATIVE price liquidation
// detection keys off (P-LIQ-1/2). Faithful port of
// `matcher/liquidation.rs::worse_of_health_price` + `health_price_with_staleness`.
// A LONG ignores an UNSET oracle (`oracle == 0`) — otherwise it would read as
// price 0 (max loss) and wrongfully liquidate. `source`: 0 = mark, 1 = oracle,
// 2 = equal. Pure (u64 comparisons only) → host-tested + Kani-proven.
// ─────────────────────────────────────────────────────────────────────────────

/// Worse-of-(mark, oracle) health price for a position.
#[inline]
pub fn worse_of_health_price(mark_t: u64, oracle_t: u64, is_long: bool) -> (u64, u8) {
    if is_long {
        if oracle_t > 0 && oracle_t < mark_t {
            (oracle_t, 1)
        } else if oracle_t > 0 && oracle_t == mark_t {
            (mark_t, 2)
        } else {
            (mark_t, 0)
        }
    } else if oracle_t > mark_t {
        (oracle_t, 1)
    } else if oracle_t == mark_t {
        (mark_t, 2)
    } else {
        (mark_t, 0)
    }
}

/// Source tag when a STALE mark forces the oracle-only fallback (ER-stall).
/// Distinct from the worse-of tags (0/1/2) so keepers/UIs can show
/// "liquidated on oracle-only (ER stalled)".
pub const HP_SOURCE_ORACLE_ONLY: u8 = 3;

/// Staleness-aware health price (ER-stall defense, P-LIQ-2). FRESH mark ⇒ the
/// proven worse-of (dual-source design untouched on the happy path); STALE mark
/// ⇒ the oracle ALONE (a frozen mark must not force a wrongful liquidation);
/// STALE + no usable oracle (`oracle == 0`) ⇒ `None` (fail-safe — refuse to
/// liquidate until a price source recovers).
#[inline]
pub fn health_price_with_staleness(
    mark_t: u64,
    oracle_t: u64,
    mark_stale: bool,
    is_long: bool,
) -> Option<(u64, u8)> {
    if !mark_stale {
        return Some(worse_of_health_price(mark_t, oracle_t, is_long));
    }
    if oracle_t > 0 {
        Some((oracle_t, HP_SOURCE_ORACLE_ONLY))
    } else {
        None
    }
}

/// The synthetic liquidation close price in ticks — the oracle moved by the
/// liquidation penalty, ALWAYS against the liquidated trader. `close_side` is
/// the side the position is closed ON (the opposite of the position side):
/// closing a LONG sells (close_side = short = 1) at `oracle − penalty`; closing
/// a SHORT buys (close_side = long = 0) at `oracle + penalty`. `penalty_delta =
/// oracle · penalty_bps / 10_000`. Saturating; clamped to `u64::MAX`. Faithful
/// port of the `liquidate_position_v2` synthetic-limit math. (The JIT auction
/// may only IMPROVE on this price — never worsen it.)
#[inline]
pub fn liquidation_penalty_price(close_side: u8, oracle_ticks: u64, penalty_bps: u32) -> u64 {
    let oracle = oracle_ticks as u128;
    let penalty_delta = (oracle * penalty_bps as u128) / (BPS_DENOM as u128);
    let v = if close_side == 1 {
        // closing a long (selling) — push the fill price DOWN.
        oracle.saturating_sub(penalty_delta)
    } else {
        // closing a short (buying) — push the fill price UP.
        oracle.saturating_add(penalty_delta)
    };
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

/// Dutch-auction-scaled liquidator reward bps. With `auction_duration_slots ==
/// 0` there is no auction → the full `reward_bps`. Otherwise the reward ramps
/// LINEARLY from 0 to `reward_bps` over the auction: `eff = reward_bps ·
/// min(elapsed, duration) / duration` (so `elapsed == 0` ⇒ 0, capped at
/// `reward_bps` once `elapsed ≥ duration`). Saturating. `elapsed_slots` is
/// `now − unhealthy_since`; the caller sources it (pin's `Position` has no
/// `unhealthy_since` field yet — the instruction passes `auction_duration = 0`,
/// i.e. flat reward, until that field is carved). Faithful port of the
/// `liquidate_position_v2` Dutch-auction reward.
#[inline]
pub fn reward_bps_effective(reward_bps: u32, elapsed_slots: u64, auction_duration_slots: u64) -> u32 {
    if auction_duration_slots == 0 {
        return reward_bps;
    }
    let scale = (elapsed_slots.min(auction_duration_slots) as u128)
        .saturating_mul(BPS_DENOM as u128)
        / (auction_duration_slots as u128);
    let eff = (reward_bps as u128).saturating_mul(scale) / (BPS_DENOM as u128);
    if eff > u32::MAX as u128 { u32::MAX } else { eff as u32 }
}

/// The liquidator reward in quote lots: `notional · reward_bps_eff / 10_000`,
/// where `notional = close_size · price · tick`. Saturating, clamped to
/// `u64::MAX`. (The instruction caps the PAID reward at the funding bucket's
/// balance — this is the gross entitlement.)
#[inline]
pub fn liquidator_reward_lots(
    close_size_lots: u64,
    price_ticks: u64,
    tick_size: u64,
    reward_bps_eff: u32,
) -> u64 {
    let notional = (close_size_lots as u128)
        .saturating_mul(price_ticks as u128)
        .saturating_mul(tick_size as u128);
    let v = notional.saturating_mul(reward_bps_eff as u128) / (BPS_DENOM as u128);
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

/// FV: machine-checked correctness of the dual-source health price (Kani,
/// comparison-only → fast). The health price is always the WORSE of the two
/// real sources for the position's side, never understates risk, never invents
/// a price; a stale mark is never used. Mirrors the Anchor proofs verbatim.
#[cfg(kani)]
mod health_price_kani_proofs {
    use super::{
        cover_shortfall, health_price_with_staleness, liquidation_penalty_price,
        worse_of_health_price, HP_SOURCE_ORACLE_ONLY,
    };

    /// The insurance draw conserves value and never overpays: `covered +
    /// remaining == shortfall`, the fund pays no more than its balance, and no
    /// more than the shortfall. Comparison-only ⇒ fast.
    #[kani::proof]
    fn cover_shortfall_conserves() {
        let balance: u64 = kani::any();
        let shortfall: u64 = kani::any();
        let (covered, remaining) = cover_shortfall(balance, shortfall);
        assert!(covered <= balance);
        assert!(covered <= shortfall);
        assert!(covered + remaining == shortfall); // no overflow: covered ≤ shortfall
    }

    // NOTE: `reward_bps_effective`'s `eff ≤ reward_bps` invariant is covered by
    // the host test below, NOT a Kani proof: its `min(e,d)·BPS / duration`
    // divides by a SYMBOLIC divisor, which CBMC explores in ~7 min even
    // u32-bounded — too slow for the per-PR Kani job. The host ramp test
    // (0 / 50% / 100% / beyond) exercises the same property cheaply.

    /// The liquidation penalty always moves the close price AGAINST the trader:
    /// closing a long fills at ≤ oracle, closing a short at ≥ oracle. Bounded to
    /// u32 so `oracle · penalty_bps` stays well within u128 (no symbolic blowup).
    #[kani::proof]
    fn penalty_price_is_adverse() {
        let oracle = kani::any::<u32>() as u64;
        let penalty_bps: u32 = kani::any();
        kani::assume(penalty_bps <= 10_000); // config-bounded (≤ 100%)
        let close_side: u8 = kani::any();
        kani::assume(close_side <= 1);
        let px = liquidation_penalty_price(close_side, oracle, penalty_bps);
        if close_side == 1 {
            assert!(px <= oracle); // closing a long fills no higher than oracle
        } else {
            assert!(px >= oracle); // closing a short fills no lower than oracle
        }
    }

    /// LONG: health price ≤ mark and ≤ a live oracle (the worse/lower of the two).
    #[kani::proof]
    fn health_price_worse_for_long() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let (hp, _) = worse_of_health_price(mark, oracle, true);
        assert!(hp <= mark);
        assert!(oracle == 0 || hp <= oracle);
    }

    /// SHORT: health price ≥ mark and ≥ oracle (the worse/higher of the two).
    #[kani::proof]
    fn health_price_worse_for_short() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let (hp, _) = worse_of_health_price(mark, oracle, false);
        assert!(hp >= mark);
        assert!(hp >= oracle);
    }

    /// The health price is ALWAYS one of the two real sources — never fabricated.
    #[kani::proof]
    fn health_price_is_a_real_source() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let is_long: bool = kani::any();
        let (hp, src) = worse_of_health_price(mark, oracle, is_long);
        assert!(hp == mark || hp == oracle);
        assert!(src <= 2);
    }

    /// P-LIQ-2: a STALE mark is never used — oracle alone, or None if no oracle.
    #[kani::proof]
    fn stale_mark_falls_back_to_oracle_only() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let is_long: bool = kani::any();
        match health_price_with_staleness(mark, oracle, true, is_long) {
            Some((hp, src)) => {
                assert!(oracle > 0);
                assert!(hp == oracle);
                assert!(src == HP_SOURCE_ORACLE_ONLY);
            }
            None => assert!(oracle == 0),
        }
    }

    /// A FRESH mark leaves the dual-source design identical to the proven worse-of.
    #[kani::proof]
    fn fresh_mark_equals_worse_of() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let is_long: bool = kani::any();
        let staleness = health_price_with_staleness(mark, oracle, false, is_long);
        let worse = worse_of_health_price(mark, oracle, is_long);
        assert!(staleness == Some(worse));
    }
}

#[cfg(test)]
mod health_price_tests {
    use super::{
        health_price_with_staleness, liquidation_penalty_price, liquidator_reward_lots,
        reward_bps_effective, worse_of_health_price, HP_SOURCE_ORACLE_ONLY,
    };

    #[test]
    fn liquidator_reward_dutch_ramp_and_amount() {
        // No auction (duration 0) ⇒ flat reward bps.
        assert_eq!(reward_bps_effective(500, 0, 0), 500);
        assert_eq!(reward_bps_effective(500, 9_999, 0), 500);
        // Linear ramp: halfway through a 100-slot auction ⇒ half the reward.
        assert_eq!(reward_bps_effective(500, 50, 100), 250);
        // elapsed 0 ⇒ 0; elapsed ≥ duration ⇒ full reward.
        assert_eq!(reward_bps_effective(500, 0, 100), 0);
        assert_eq!(reward_bps_effective(500, 100, 100), 500);
        assert_eq!(reward_bps_effective(500, 1_000, 100), 500);
        // Amount: notional = 10·100·1 = 1000; at 500 bps (5%) → 50.
        assert_eq!(liquidator_reward_lots(10, 100, 1, 500), 50);
        assert_eq!(liquidator_reward_lots(10, 100, 1, 0), 0);
    }

    #[test]
    fn penalty_price_moves_against_the_trader() {
        // Closing a LONG (close_side = short = 1): oracle 100, 1% penalty → 99.
        assert_eq!(liquidation_penalty_price(1, 100, 100), 99);
        // Closing a SHORT (close_side = long = 0): oracle 100, 1% penalty → 101.
        assert_eq!(liquidation_penalty_price(0, 100, 100), 101);
        // Zero penalty ⇒ exactly the oracle, either side.
        assert_eq!(liquidation_penalty_price(1, 100, 0), 100);
        assert_eq!(liquidation_penalty_price(0, 100, 0), 100);
        // Penalty larger than the oracle saturates the long-close to 0 (never wraps).
        assert_eq!(liquidation_penalty_price(1, 100, 20_000), 0);
    }

    #[test]
    fn long_takes_lower_short_takes_higher() {
        // LONG: worse = lower; a higher / unset oracle is ignored.
        assert_eq!(worse_of_health_price(100, 90, true), (90, 1));
        assert_eq!(worse_of_health_price(100, 110, true), (100, 0));
        assert_eq!(worse_of_health_price(100, 0, true), (100, 0));
        // SHORT: worse = higher; a lower oracle is ignored.
        assert_eq!(worse_of_health_price(100, 110, false), (110, 1));
        assert_eq!(worse_of_health_price(100, 90, false), (100, 0));
        // Equal sources ⇒ source tag 2.
        assert_eq!(worse_of_health_price(100, 100, true), (100, 2));
    }

    #[test]
    fn fresh_mark_uses_worse_of_both_sides() {
        assert_eq!(
            health_price_with_staleness(100, 90, false, true),
            Some(worse_of_health_price(100, 90, true))
        );
        assert_eq!(
            health_price_with_staleness(100, 110, false, false),
            Some(worse_of_health_price(100, 110, false))
        );
    }

    #[test]
    fn stale_mark_ignores_adverse_frozen_mark() {
        // Frozen mark 50 would liquidate a long far below the live oracle 100;
        // stale ⇒ drop the mark, price off the oracle alone.
        assert_eq!(
            health_price_with_staleness(50, 100, true, true),
            Some((100, HP_SOURCE_ORACLE_ONLY))
        );
        // Stale + no oracle ⇒ fail-safe None (refuse to liquidate).
        assert_eq!(health_price_with_staleness(50, 0, true, true), None);
    }
}
