//! Leverage-tier ladder validation — pure, host-tested, anchor-parity port of
//! `validate_leverage_tiers`.
//!
//! A market's maintenance-margin requirement (MMR) can rise with position size:
//! the ladder maps an ascending notional threshold to the MMR that applies at or
//! above it. The base market MMR (`market.maintenance_margin_bps`) is the floor —
//! a tier may only ever *raise* the requirement, never lower it below base.

use crate::constants::BPS_DENOM;
use crate::risk::tiered_mmr_bps;
use crate::state::{MarketLeverageTiers, MAX_LEVERAGE_TIERS};

/// Validate a proposed ladder of `(min_notional_quote_lots, mmr_bps)` rungs
/// against `base_mmr` (= `market.maintenance_margin_bps`). Returns `Err(())` on
/// any violation:
///  * empty, or more than `MAX_LEVERAGE_TIERS` rungs;
///  * an `mmr_bps` below `base_mmr` (would *weaken* margin) or above `BPS_DENOM`
///    (100% — nonsensical);
///  * `min_notional_quote_lots` not STRICTLY ascending (ambiguous lookup).
pub fn validate_tiers(base_mmr: u32, tiers: &[(u64, u32)]) -> Result<(), ()> {
    if tiers.is_empty() || tiers.len() > MAX_LEVERAGE_TIERS {
        return Err(());
    }
    let mut prev_min: Option<u64> = None;
    for &(min_notional, mmr) in tiers {
        if mmr < base_mmr || mmr > BPS_DENOM {
            return Err(());
        }
        if let Some(prev) = prev_min {
            if min_notional <= prev {
                return Err(());
            }
        }
        prev_min = Some(min_notional);
    }
    Ok(())
}

/// Parse the instruction wire format into a fixed stack buffer (no alloc):
/// `[tier_count: u8] [ (min_notional: u64 LE)(mmr_bps: u32 LE) ; tier_count ]`.
/// Returns the rung count. `Err(())` if empty, over `MAX_LEVERAGE_TIERS`, or the
/// buffer is too short for the declared count.
pub fn parse_tiers(
    data: &[u8],
    out: &mut [(u64, u32); MAX_LEVERAGE_TIERS],
) -> Result<usize, ()> {
    let count = *data.first().ok_or(())? as usize;
    if count == 0 || count > MAX_LEVERAGE_TIERS {
        return Err(());
    }
    if data.len() < 1 + count * 12 {
        return Err(());
    }
    for (i, slot) in out.iter_mut().enumerate().take(count) {
        let off = 1 + i * 12;
        let mut mn = [0u8; 8];
        mn.copy_from_slice(&data[off..off + 8]);
        let mut mr = [0u8; 4];
        mr.copy_from_slice(&data[off + 8..off + 12]);
        *slot = (u64::from_le_bytes(mn), u32::from_le_bytes(mr));
    }
    Ok(count)
}

/// Resolve the effective maintenance base MMR for a position of `notional` quote
/// lots, given the market's flat `base_mmr` and its on-chain tier table. Reads at
/// most `MAX_LEVERAGE_TIERS` rungs (clamping a corrupt `tier_count`) into a stack
/// buffer and delegates to the proven `tiered_mmr_bps`. An empty table returns
/// `base_mmr` unchanged.
pub fn resolve_base_mmr(base_mmr: u32, tiers: &MarketLeverageTiers, notional: u128) -> u32 {
    let count = (tiers.tier_count as usize).min(MAX_LEVERAGE_TIERS);
    let mut buf = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
    for (i, slot) in buf.iter_mut().enumerate().take(count) {
        *slot = (
            tiers.tiers[i].min_notional_quote_lots,
            tiers.tiers[i].mmr_bps,
        );
    }
    tiered_mmr_bps(base_mmr, &buf[..count], notional)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LeverageTier, LEVERAGE_TIERS_DISC};

    fn tiers_account(rungs: &[(u64, u32)]) -> MarketLeverageTiers {
        let mut t = MarketLeverageTiers {
            disc: LEVERAGE_TIERS_DISC,
            market: [0u8; 32],
            bump: 0,
            tier_count: rungs.len() as u8,
            _pad0: [0u8; 6],
            tiers: [LeverageTier { min_notional_quote_lots: 0, mmr_bps: 0, _pad: [0u8; 4] };
                MAX_LEVERAGE_TIERS],
        };
        for (i, &(mn, mmr)) in rungs.iter().enumerate() {
            t.tiers[i] = LeverageTier { min_notional_quote_lots: mn, mmr_bps: mmr, _pad: [0u8; 4] };
        }
        t
    }

    #[test]
    fn resolve_picks_the_position_tier_and_falls_back_to_base() {
        let t = tiers_account(&[(1_000_000, 200), (5_000_000, 300), (25_000_000, 500)]);
        assert_eq!(resolve_base_mmr(100, &t, 999_999), 100); // below first rung → base
        assert_eq!(resolve_base_mmr(100, &t, 1_000_000), 200);
        assert_eq!(resolve_base_mmr(100, &t, 4_999_999), 200);
        assert_eq!(resolve_base_mmr(100, &t, 5_000_000), 300);
        assert_eq!(resolve_base_mmr(100, &t, u128::MAX), 500);
    }

    #[test]
    fn resolve_empty_table_returns_base() {
        let t = tiers_account(&[]);
        assert_eq!(resolve_base_mmr(123, &t, u128::MAX), 123);
    }

    #[test]
    fn resolve_clamps_a_corrupt_tier_count() {
        // tier_count claims 200 but the array is fixed at MAX_LEVERAGE_TIERS;
        // the clamp guarantees no out-of-bounds read.
        let mut t = tiers_account(&[(1_000_000, 400)]);
        t.tier_count = 200;
        let _ = resolve_base_mmr(100, &t, u128::MAX);
    }

    #[test]
    fn parse_round_trips_a_two_tier_ladder() {
        // count=2; (0, 500), (10_000, 800)
        let mut data = vec![2u8];
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&500u32.to_le_bytes());
        data.extend_from_slice(&10_000u64.to_le_bytes());
        data.extend_from_slice(&800u32.to_le_bytes());
        let mut out = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
        assert_eq!(parse_tiers(&data, &mut out), Ok(2));
        assert_eq!(out[0], (0, 500));
        assert_eq!(out[1], (10_000, 800));
    }

    #[test]
    fn parse_rejects_empty_overlong_count_and_short_buffer() {
        let mut out = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
        assert_eq!(parse_tiers(&[], &mut out), Err(())); // no count byte
        assert_eq!(parse_tiers(&[0], &mut out), Err(())); // zero tiers
        assert_eq!(parse_tiers(&[(MAX_LEVERAGE_TIERS + 1) as u8], &mut out), Err(()));
        assert_eq!(parse_tiers(&[1, 0, 0, 0], &mut out), Err(())); // truncated rung
    }

    #[test]
    fn accepts_a_well_formed_ascending_ladder() {
        let base = 500;
        let tiers = [(0u64, 500u32), (10_000, 800), (100_000, 1_500)];
        assert_eq!(validate_tiers(base, &tiers), Ok(()));
    }

    #[test]
    fn rejects_empty_or_oversized() {
        assert_eq!(validate_tiers(500, &[]), Err(()));
        let too_many: [(u64, u32); MAX_LEVERAGE_TIERS + 1] =
            core::array::from_fn(|i| (i as u64, 600));
        assert_eq!(validate_tiers(500, &too_many), Err(()));
    }

    #[test]
    fn rejects_mmr_below_base_or_above_full() {
        // below base — would weaken margin.
        assert_eq!(validate_tiers(500, &[(0, 499)]), Err(()));
        // above 100%.
        assert_eq!(validate_tiers(500, &[(0, BPS_DENOM + 1)]), Err(()));
        // exactly base and exactly full are both allowed.
        assert_eq!(validate_tiers(500, &[(0, 500)]), Ok(()));
        assert_eq!(validate_tiers(500, &[(0, BPS_DENOM)]), Ok(()));
    }

    #[test]
    fn rejects_non_strictly_ascending_notional() {
        // equal notionals — ambiguous.
        assert_eq!(validate_tiers(500, &[(10_000, 600), (10_000, 700)]), Err(()));
        // descending.
        assert_eq!(validate_tiers(500, &[(10_000, 600), (5_000, 700)]), Err(()));
    }

    #[test]
    fn accepts_exactly_max_tiers() {
        let full: [(u64, u32); MAX_LEVERAGE_TIERS] =
            core::array::from_fn(|i| (i as u64 * 1_000, 600));
        assert_eq!(validate_tiers(500, &full), Ok(()));
    }
}
