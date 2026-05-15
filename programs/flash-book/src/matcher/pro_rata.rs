//! Pro-rata fill split (Wave 59).
//!
//! When multiple makers rest at the same price level, split incoming
//! taker liquidity proportionally to their displayed size — instead of
//! FIFO. Optional per-market policy; FIFO remains the default (lowest
//! latency for the first maker).
//!
//! Pro-rata fits markets where fairness across makers is valued over
//! first-come-first-served. CME does this on options; some DEXes
//! offer it as a toggle.
//!
//! Pure math. Wire-in (Wave 59b) calls this from the matcher walk
//! when `market.params.matching_policy == MatchingPolicy::ProRata`.

/// Split `fill_size` proportionally across N makers with sizes
/// `maker_sizes`. Returns a Vec of `(maker_index, fill_lots)`.
///
/// Pure function. Floor-rounds; any residual lots (from rounding)
/// are assigned to the first maker (deterministic tiebreak).
pub fn split_pro_rata(fill_size: u64, maker_sizes: &[u64]) -> Vec<(usize, u64)> {
    if fill_size == 0 || maker_sizes.is_empty() {
        return Vec::new();
    }
    let total: u128 = maker_sizes.iter().map(|s| *s as u128).sum();
    if total == 0 {
        return Vec::new();
    }
    let fill_u128 = fill_size as u128;
    let mut splits: Vec<(usize, u64)> = Vec::with_capacity(maker_sizes.len());
    let mut assigned: u128 = 0;
    for (i, size) in maker_sizes.iter().enumerate() {
        let share = ((*size as u128) * fill_u128 / total).min(u64::MAX as u128) as u64;
        if share > 0 {
            splits.push((i, share));
            assigned = assigned.saturating_add(share as u128);
        }
    }
    // Residual (from flooring) → first maker.
    if assigned < fill_u128 {
        let residual = (fill_u128 - assigned).min(u64::MAX as u128) as u64;
        if let Some(first) = splits.first_mut() {
            first.1 = first.1.saturating_add(residual);
        } else if !maker_sizes.is_empty() {
            splits.push((0, residual));
        }
    }
    splits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_makers_empty_result() {
        let r = split_pro_rata(100, &[]);
        assert!(r.is_empty());
    }

    #[test]
    fn zero_fill_empty_result() {
        let r = split_pro_rata(0, &[100, 200]);
        assert!(r.is_empty());
    }

    #[test]
    fn equal_makers_split_equally() {
        let r = split_pro_rata(100, &[50, 50]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100);
        assert_eq!(r.len(), 2);
        // Each gets 50.
        assert_eq!(r[0].1, 50);
        assert_eq!(r[1].1, 50);
    }

    #[test]
    fn unequal_makers_split_proportionally() {
        // Makers: 75, 25 → 75%/25% of 100 = 75, 25.
        let r = split_pro_rata(100, &[75, 25]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100);
        assert_eq!(r[0].1, 75);
        assert_eq!(r[1].1, 25);
    }

    #[test]
    fn residual_lots_go_to_first_maker() {
        // 10 lots, makers 3+3+3 = 9 total. Each gets 10×3/9 = 3 (floor).
        // 9 assigned, 1 residual → first gets +1 → (4, 3, 3).
        let r = split_pro_rata(10, &[3, 3, 3]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 10);
        assert_eq!(r[0].1, 4);
        assert_eq!(r[1].1, 3);
        assert_eq!(r[2].1, 3);
    }

    #[test]
    fn fill_larger_than_total_capped_implicitly() {
        // If fill exceeds total maker size, split proportionally to total.
        // 200 fill, makers 50+50 → each gets 200×50/100 = 100.
        let r = split_pro_rata(200, &[50, 50]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 200);
        assert_eq!(r[0].1, 100);
        assert_eq!(r[1].1, 100);
    }

    #[test]
    fn single_maker_gets_full_fill() {
        let r = split_pro_rata(100, &[200]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, 100);
    }
}
