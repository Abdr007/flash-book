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
