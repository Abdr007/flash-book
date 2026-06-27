//! Fee-tier resolution — pure math, ported verbatim from the Anchor program's
//! `matcher/risk.rs::resolve_fee_tier` and `lib.rs::tier_index_for_volume`.
//!
//! Volume-based maker-rebate / taker-fee tiers: a trader's 30-day quote volume
//! selects the highest tier whose `min_volume` it meets. `tiers` MUST be sorted
//! ascending by `min_volume` (the program enforces this when the FeeTiers
//! account is written); the loop relies on that ordering and `break`s at the
//! first tier the volume does not reach.
//!
//! Tuple layout matches the Anchor call sites: `(min_volume_quote_lots,
//! maker_rebate_bps, taker_fee_bps)`.

/// Resolve `(maker_rebate_bps, taker_fee_bps)` for a trader's volume, falling
/// back to the market defaults when no tier applies. Exact port of
/// `matcher::risk::resolve_fee_tier`.
pub fn resolve_fee_tier(
    default_maker_rebate_bps: i32,
    default_taker_fee_bps: u32,
    tiers: &[(u64, i32, u32)],
    trader_volume_quote_lots: u64,
) -> (i32, u32) {
    let mut maker = default_maker_rebate_bps;
    let mut taker = default_taker_fee_bps;
    for (min_vol, m, t) in tiers {
        if trader_volume_quote_lots >= *min_vol {
            maker = *m;
            taker = *t;
        } else {
            break;
        }
    }
    (maker, taker)
}

/// 0-based index of the highest tier the volume qualifies for (0 when none /
/// empty). Exact port of `tier_index_for_volume`. Used for fee-tier
/// promotion/demotion events; pre-fill volume is passed so a trade can't
/// promote itself.
pub fn tier_index_for_volume(pairs: &[(u64, i32, u32)], volume: u64) -> u8 {
    let mut idx: u8 = 0;
    for (i, (min_vol, _, _)) in pairs.iter().enumerate() {
        if volume >= *min_vol {
            idx = i as u8;
        } else {
            break;
        }
    }
    idx
}

/// Apply a per-trader taker-fee discount (bps) to a gross fee. Clamped: a
/// discount ≥ BPS_DENOM yields 0, and the bps is capped at BPS_DENOM so the
/// subtraction never underflows. u128 intermediate so `gross × bps` can't
/// overflow. Pure + host-tested.
#[inline]
pub fn discounted_fee(gross_fee: u64, discount_bps: u32) -> u64 {
    let denom = crate::constants::BPS_DENOM as u128;
    let d = (discount_bps as u128).min(denom);
    (((gross_fee as u128) * (denom - d)) / denom) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discount_applies_and_clamps() {
        // no discount → gross unchanged
        assert_eq!(discounted_fee(1_000, 0), 1_000);
        // 50% off
        assert_eq!(discounted_fee(1_000, 5_000), 500);
        // 100% off → free
        assert_eq!(discounted_fee(1_000, 10_000), 0);
        // over-cap clamps to 100% off (no underflow), never negative
        assert_eq!(discounted_fee(1_000, 99_999), 0);
        // large gross doesn't overflow the u128 intermediate
        assert_eq!(discounted_fee(u64::MAX, 0), u64::MAX);
    }

    // Mirrors the Anchor `fee_tier_tests` so the ported math is exercised
    // with the same vectors.
    #[test]
    fn empty_tiers_returns_defaults() {
        assert_eq!(resolve_fee_tier(2, 5, &[], 1_000_000_000), (2, 5));
    }

    #[test]
    fn below_first_tier_returns_defaults() {
        let tiers = [(1_000u64, -1i32, 3u32), (10_000, -2, 2)];
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 999), (2, 5));
    }

    #[test]
    fn picks_highest_qualifying_tier() {
        let tiers = [(1_000u64, -1i32, 3u32), (10_000, -2, 2), (100_000, -3, 1)];
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 1_000), (-1, 3));
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 50_000), (-2, 2));
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 100_000), (-3, 1));
        assert_eq!(resolve_fee_tier(2, 5, &tiers, u64::MAX), (-3, 1));
    }

    #[test]
    fn exact_boundary_qualifies() {
        let tiers = [(1_000u64, -1i32, 3u32)];
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 1_000), (-1, 3));
        assert_eq!(resolve_fee_tier(2, 5, &tiers, 999), (2, 5));
    }

    #[test]
    fn tier_index_tracks_volume() {
        let pairs = [(1_000u64, -1i32, 3u32), (10_000, -2, 2), (100_000, -3, 1)];
        assert_eq!(tier_index_for_volume(&pairs, 0), 0);
        assert_eq!(tier_index_for_volume(&pairs, 1_000), 0);
        assert_eq!(tier_index_for_volume(&pairs, 10_000), 1);
        assert_eq!(tier_index_for_volume(&pairs, 100_000), 2);
        assert_eq!(tier_index_for_volume(&[], 5), 0);
    }

    #[test]
    fn resolve_and_index_agree_on_winning_tier() {
        // The tier resolve_fee_tier picks must be the tier_index_for_volume index.
        let pairs = [(1_000u64, -1i32, 3u32), (10_000, -2, 2), (100_000, -3, 1)];
        for vol in [0u64, 500, 1_000, 9_999, 10_000, 99_999, 100_000, u64::MAX] {
            let (m, t) = resolve_fee_tier(2, 5, &pairs, vol);
            let idx = tier_index_for_volume(&pairs, vol);
            if vol < pairs[0].0 {
                assert_eq!((m, t), (2, 5)); // defaults; idx stays 0
            } else {
                let (_, em, et) = pairs[idx as usize];
                assert_eq!((m, t), (em, et));
            }
        }
    }
}
