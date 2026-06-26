//! Pro-rata fill split (Wave 59) — `no_std` port.
//!
//! When multiple makers rest at the same price level, split incoming taker
//! liquidity proportionally to their displayed size instead of FIFO.
//!
//! Adapted from the Anchor `split_pro_rata` (which returned a `Vec`): writes the
//! `(maker_index, fill_lots)` pairs into a **caller-provided buffer** and returns
//! the count written. The math is otherwise identical — floor-rounding, with the
//! residual from flooring assigned to the first non-zero maker (deterministic
//! tiebreak). Pairs with a zero share are omitted, matching the Vec version.

/// Split `fill_size` proportionally across `maker_sizes`, writing
/// `(maker_index, fill_lots)` into `out` and returning the number written.
/// `out` should have capacity `>= maker_sizes.len()`; if it is shorter, the
/// extra non-zero pairs are dropped (the returned count reflects what fit).
pub fn split_pro_rata(fill_size: u64, maker_sizes: &[u64], out: &mut [(usize, u64)]) -> usize {
    if fill_size == 0 || maker_sizes.is_empty() {
        return 0;
    }
    let total: u128 = maker_sizes.iter().map(|s| *s as u128).sum();
    if total == 0 {
        return 0;
    }
    let fill_u128 = fill_size as u128;
    // M2 (audit 2026-06): a maker is never filled past its OWN size, and the
    // level fills at most min(fill_size, Σ sizes); the remainder is unfillable
    // taker liquidity and is dropped, NOT crammed onto the first maker. Mirrors
    // the Anchor `split_pro_rata` fix. The caller buffer (cap ≥ maker count) doubles
    // as index-addressable scratch: write every maker's capped share, distribute
    // the flooring residual by remaining capacity, then compact out the zeros.
    let target = fill_u128.min(total);
    let m = maker_sizes.len().min(out.len());

    let mut assigned: u128 = 0;
    for i in 0..m {
        let size = maker_sizes[i] as u128;
        let share = (size * fill_u128 / total).min(size);
        out[i] = (i, share.min(u64::MAX as u128) as u64);
        assigned = assigned.saturating_add(share);
    }
    let mut residual = target.saturating_sub(assigned);
    for i in 0..m {
        if residual == 0 {
            break;
        }
        let capacity = (maker_sizes[i] as u128).saturating_sub(out[i].1 as u128);
        let add = capacity.min(residual);
        out[i].1 = out[i].1.saturating_add(add.min(u64::MAX as u128) as u64);
        residual = residual.saturating_sub(add);
    }
    // Compact non-zero fills to the front, preserving index order.
    let mut n: usize = 0;
    for i in 0..m {
        if out[i].1 > 0 {
            out[n] = out[i];
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same vectors as the Anchor `pro_rata::tests`, adapted to the buffer API.
    fn run(fill: u64, makers: &[u64]) -> ([(usize, u64); 8], usize) {
        let mut out = [(0usize, 0u64); 8];
        let n = split_pro_rata(fill, makers, &mut out);
        (out, n)
    }

    #[test]
    fn empty_makers_empty_result() {
        let (_, n) = run(100, &[]);
        assert_eq!(n, 0);
    }

    #[test]
    fn zero_fill_empty_result() {
        let (_, n) = run(0, &[100, 200]);
        assert_eq!(n, 0);
    }

    #[test]
    fn equal_makers_split_equally() {
        let (out, n) = run(100, &[50, 50]);
        assert_eq!(n, 2);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100);
        assert_eq!(out[0].1, 50);
        assert_eq!(out[1].1, 50);
    }

    #[test]
    fn unequal_makers_split_proportionally() {
        let (out, n) = run(100, &[75, 25]);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100);
        assert_eq!(out[0].1, 75);
        assert_eq!(out[1].1, 25);
    }

    #[test]
    fn flooring_residual_goes_to_first_maker_with_capacity() {
        // 11 lots, makers 7+7=14, each floors to 5, residual 1 → first (cap 2) +1.
        let (out, n) = run(11, &[7, 7]);
        assert_eq!(n, 2);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 11);
        assert_eq!(out[0].1, 6);
        assert_eq!(out[1].1, 5);
    }

    #[test]
    fn fill_larger_than_total_caps_each_maker_at_its_size() {
        // M2: fill exceeds total displayed (200 > 100) → each maker fills its
        // full size (50) and no more; the extra 100 is dropped. Old: (100, 100).
        let (out, n) = run(200, &[50, 50]);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 100);
        assert_eq!(out[0].1, 50);
        assert_eq!(out[1].1, 50);
    }

    #[test]
    fn single_maker_gets_full_fill() {
        let (out, n) = run(100, &[200]);
        assert_eq!(n, 1);
        assert_eq!(out[0].1, 100);
    }

    #[test]
    fn all_shares_floor_to_zero_residual_to_first() {
        // fill 1, makers 1+1+1=3 → each 1*1/3 = 0; residual 1 → (0, 1).
        let (out, n) = run(1, &[1, 1, 1]);
        assert_eq!(n, 1);
        assert_eq!(out[0], (0, 1));
    }
}
