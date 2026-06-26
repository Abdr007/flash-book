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
/// `maker_sizes`. Returns a Vec of `(maker_index, fill_lots)` in index order.
///
/// Pure function. Two invariants the matcher relies on for value conservation
/// (M2, audit 2026-06):
///   1. No maker is ever filled beyond its OWN displayed size — a maker cannot
///      be forced into a larger fill than it rested.
///   2. The level fills at most `min(fill_size, Σ maker_sizes)` lots total; any
///      remainder is genuine unfillable taker liquidity and is dropped here (the
///      caller leaves it as the taker residual), NOT crammed onto a maker.
/// Floor-rounds the proportional share; the flooring residual is distributed to
/// makers with REMAINING capacity in index order (deterministic), never past a
/// maker's size.
pub fn split_pro_rata(fill_size: u64, maker_sizes: &[u64]) -> Vec<(usize, u64)> {
    if fill_size == 0 || maker_sizes.is_empty() {
        return Vec::new();
    }
    let total: u128 = maker_sizes.iter().map(|s| *s as u128).sum();
    if total == 0 {
        return Vec::new();
    }
    let fill_u128 = fill_size as u128;
    // The level can fill no more than every maker's full displayed size.
    let target = fill_u128.min(total);

    let mut fills: Vec<u128> = vec![0; maker_sizes.len()];
    let mut assigned: u128 = 0;
    for (i, size) in maker_sizes.iter().enumerate() {
        // Floor proportional share, hard-capped at the maker's own size.
        let share = ((*size as u128) * fill_u128 / total).min(*size as u128);
        fills[i] = share;
        assigned = assigned.saturating_add(share);
    }
    // Distribute the flooring residual ONLY to makers that still have capacity,
    // in index order. `target ≤ total` and total capacity == `total`, so the
    // residual is always fully placed; the loop just stops early once it is.
    let mut residual = target.saturating_sub(assigned);
    for (i, size) in maker_sizes.iter().enumerate() {
        if residual == 0 {
            break;
        }
        let capacity = (*size as u128).saturating_sub(fills[i]);
        let add = capacity.min(residual);
        fills[i] = fills[i].saturating_add(add);
        residual = residual.saturating_sub(add);
    }

    maker_sizes
        .iter()
        .enumerate()
        .filter_map(|(i, _)| {
            if fills[i] > 0 {
                Some((i, fills[i].min(u64::MAX as u128) as u64))
            } else {
                None
            }
        })
        .collect()
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
    fn flooring_residual_goes_to_first_maker_with_capacity() {
        // 11 lots, makers 7+7 = 14 total. Each gets floor(11×7/14) = 5 (floor),
        // 10 assigned, 1 residual → first maker (capacity 2) absorbs it → (6, 5).
        // Neither maker exceeds its size of 7.
        let r = split_pro_rata(11, &[7, 7]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 11);
        assert_eq!(r[0].1, 6);
        assert_eq!(r[1].1, 5);
    }

    #[test]
    fn fill_larger_than_total_caps_each_maker_at_its_size() {
        // M2: fill exceeds total displayed (200 > 100). Each maker fills its FULL
        // size (50) and NO MORE — the extra 100 lots are unfillable and dropped,
        // NOT crammed onto a maker. Old behaviour wrongly returned (100, 100).
        let r = split_pro_rata(200, &[50, 50]);
        let total: u64 = r.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100, "level fills at most Σ maker sizes");
        assert_eq!(r[0].1, 50);
        assert_eq!(r[1].1, 50);
    }

    #[test]
    fn no_maker_ever_overfilled_beyond_its_size() {
        // M2 invariant sweep: across assorted fills and maker books, every
        // maker's fill is ≤ its displayed size and the total ≤ min(fill, Σsizes).
        let books: &[&[u64]] = &[&[3, 3, 3], &[1, 100], &[10, 1, 1], &[5], &[7, 7, 1]];
        for book in books {
            let sigma: u64 = book.iter().sum();
            for fill in [0u64, 1, 2, 9, 10, 11, 100, 500] {
                let r = split_pro_rata(fill, book);
                let mut got = 0u64;
                for (i, f) in &r {
                    assert!(*f <= book[*i], "maker {i} overfilled: {f} > {}", book[*i]);
                    got += f;
                }
                assert!(got <= fill.min(sigma), "level overfilled: {got} > min({fill},{sigma})");
                // Conservation: when there is fillable liquidity, fill it exactly.
                assert_eq!(got, fill.min(sigma), "must fill exactly min(fill, Σsizes)");
            }
        }
    }

    #[test]
    fn single_maker_gets_full_fill() {
        let r = split_pro_rata(100, &[200]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, 100);
    }
}
