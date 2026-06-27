//! Volume-based fee-tier table validation — pure, host-tested, anchor-parity port
//! of `validate_fee_tiers`.
//!
//! The table maps a trader's rolling volume to `(maker_rebate_bps,
//! taker_fee_bps)`. Rungs are ascending by volume; as volume rises the taker fee
//! may only fall (never rise) and the maker rebate may only improve (never
//! worsen) — so a higher-volume trader is never treated worse than a lower one.

use crate::constants::MAX_FEE_TIER_BPS;
use crate::state::MAX_FEE_TIERS;

/// A parsed rung: `(min_volume_quote_lots, maker_rebate_bps, taker_fee_bps)`.
pub type FeeRung = (u64, i32, u32);

/// Validate a proposed fee-tier table. `Err(())` on any violation:
///  * `volume_window_slots == 0`, empty, or more than `MAX_FEE_TIERS` rungs;
///  * the first rung's `min_volume` is not 0 (the table must cover zero volume);
///  * any `taker_fee_bps` or `|maker_rebate_bps|` over `MAX_FEE_TIER_BPS`;
///  * `min_volume` not STRICTLY ascending;
///  * `taker_fee_bps` rising as volume rises (must be non-increasing);
///  * `maker_rebate_bps` worsening as volume rises (must be non-decreasing).
pub fn validate_fee_tiers(volume_window_slots: u64, tiers: &[FeeRung]) -> Result<(), ()> {
    if volume_window_slots == 0 || tiers.is_empty() || tiers.len() > MAX_FEE_TIERS {
        return Err(());
    }
    if tiers[0].0 != 0 {
        return Err(());
    }
    let mut prev: Option<FeeRung> = None;
    for &(min_vol, maker, taker) in tiers {
        if taker > MAX_FEE_TIER_BPS || maker.unsigned_abs() > MAX_FEE_TIER_BPS {
            return Err(());
        }
        if let Some((p_min, p_maker, p_taker)) = prev {
            if min_vol <= p_min {
                return Err(()); // strictly ascending volume
            }
            if taker > p_taker {
                return Err(()); // taker fee must not rise
            }
            if maker < p_maker {
                return Err(()); // maker rebate must not worsen
            }
        }
        prev = Some((min_vol, maker, taker));
    }
    Ok(())
}

/// Parse the wire format into a fixed stack buffer (no alloc):
/// `[ (min_volume: u64 LE)(maker_rebate: i32 LE)(taker_fee: u32 LE) ; tier_count ]`.
/// `tier_count` is supplied separately (the caller reads it from the header).
/// Returns the rung count, or `Err(())` if empty, over `MAX_FEE_TIERS`, or short.
pub fn parse_fee_tiers(
    tier_count: usize,
    data: &[u8],
    out: &mut [FeeRung; MAX_FEE_TIERS],
) -> Result<usize, ()> {
    if tier_count == 0 || tier_count > MAX_FEE_TIERS {
        return Err(());
    }
    if data.len() < tier_count * 16 {
        return Err(());
    }
    for (i, slot) in out.iter_mut().enumerate().take(tier_count) {
        let off = i * 16;
        let mut v = [0u8; 8];
        v.copy_from_slice(&data[off..off + 8]);
        let mut m = [0u8; 4];
        m.copy_from_slice(&data[off + 8..off + 12]);
        let mut t = [0u8; 4];
        t.copy_from_slice(&data[off + 12..off + 16]);
        *slot = (
            u64::from_le_bytes(v),
            i32::from_le_bytes(m),
            u32::from_le_bytes(t),
        );
    }
    Ok(tier_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_formed_descending_fee_ladder() {
        let tiers = [(0u64, -5i32, 30u32), (1_000_000, 0, 20), (10_000_000, 5, 10)];
        assert_eq!(validate_fee_tiers(900, &tiers), Ok(()));
    }

    #[test]
    fn rejects_zero_window_empty_oversized_or_nonzero_first() {
        assert_eq!(validate_fee_tiers(0, &[(0, 0, 10)]), Err(())); // window 0
        assert_eq!(validate_fee_tiers(900, &[]), Err(())); // empty
        assert_eq!(validate_fee_tiers(900, &[(1, 0, 10)]), Err(())); // first not zero-vol
        let big: [FeeRung; MAX_FEE_TIERS + 1] = core::array::from_fn(|i| (i as u64, 0, 10));
        assert_eq!(validate_fee_tiers(900, &big), Err(()));
    }

    #[test]
    fn rejects_caps_and_non_monotone() {
        // taker over cap.
        assert_eq!(validate_fee_tiers(900, &[(0, 0, MAX_FEE_TIER_BPS + 1)]), Err(()));
        // |maker| over cap.
        assert_eq!(
            validate_fee_tiers(900, &[(0, -(MAX_FEE_TIER_BPS as i32) - 1, 10)]),
            Err(())
        );
        // taker rising with volume.
        assert_eq!(validate_fee_tiers(900, &[(0, 0, 10), (5, 0, 20)]), Err(()));
        // maker worsening with volume.
        assert_eq!(validate_fee_tiers(900, &[(0, 5, 10), (5, -5, 10)]), Err(()));
        // volume not strictly ascending.
        assert_eq!(validate_fee_tiers(900, &[(0, 0, 10), (0, 0, 10)]), Err(()));
    }

    #[test]
    fn parse_round_trips_two_rungs() {
        let mut data = Vec::new();
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&(-5i32).to_le_bytes());
        data.extend_from_slice(&30u32.to_le_bytes());
        data.extend_from_slice(&1_000_000u64.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&20u32.to_le_bytes());
        let mut out = [(0u64, 0i32, 0u32); MAX_FEE_TIERS];
        assert_eq!(parse_fee_tiers(2, &data, &mut out), Ok(2));
        assert_eq!(out[0], (0, -5, 30));
        assert_eq!(out[1], (1_000_000, 0, 20));
    }

    #[test]
    fn parse_rejects_bad_count_or_short_buffer() {
        let mut out = [(0u64, 0i32, 0u32); MAX_FEE_TIERS];
        assert_eq!(parse_fee_tiers(0, &[0u8; 16], &mut out), Err(()));
        assert_eq!(parse_fee_tiers(MAX_FEE_TIERS + 1, &[0u8; 999], &mut out), Err(()));
        assert_eq!(parse_fee_tiers(1, &[0u8; 15], &mut out), Err(())); // short
    }
}
