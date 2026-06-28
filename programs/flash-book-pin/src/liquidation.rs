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

#[cfg(test)]
mod tests {
    use super::*;

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

/// FV: machine-checked correctness of the dual-source health price (Kani,
/// comparison-only → fast). The health price is always the WORSE of the two
/// real sources for the position's side, never understates risk, never invents
/// a price; a stale mark is never used. Mirrors the Anchor proofs verbatim.
#[cfg(kani)]
mod health_price_kani_proofs {
    use super::{health_price_with_staleness, worse_of_health_price, HP_SOURCE_ORACLE_ONLY};

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
    use super::{health_price_with_staleness, worse_of_health_price, HP_SOURCE_ORACLE_ONLY};

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
