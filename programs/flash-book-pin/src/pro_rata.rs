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
    let mut n: usize = 0;
    let mut assigned: u128 = 0;
    for (i, size) in maker_sizes.iter().enumerate() {
        let share = ((*size as u128) * fill_u128 / total).min(u64::MAX as u128) as u64;
        if share > 0 {
            if n < out.len() {
                out[n] = (i, share);
                n += 1;
            }
            assigned = assigned.saturating_add(share as u128);
        }
    }
    // Residual (from flooring) → first non-zero maker.
    if assigned < fill_u128 {
        let residual = (fill_u128 - assigned).min(u64::MAX as u128) as u64;
        if n > 0 {
            out[0].1 = out[0].1.saturating_add(residual);
        } else if !out.is_empty() {
            out[0] = (0, residual);
            n = 1;
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
    fn residual_lots_go_to_first_maker() {
        // 10 lots, makers 3+3+3=9, each floors to 3, residual 1 → first +1.
        let (out, n) = run(10, &[3, 3, 3]);
        assert_eq!(n, 3);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 10);
        assert_eq!(out[0].1, 4);
        assert_eq!(out[1].1, 3);
        assert_eq!(out[2].1, 3);
    }

    #[test]
    fn fill_larger_than_total() {
        let (out, n) = run(200, &[50, 50]);
        let total: u64 = out[..n].iter().map(|(_, s)| s).sum();
        assert_eq!(total, 200);
        assert_eq!(out[0].1, 100);
        assert_eq!(out[1].1, 100);
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
