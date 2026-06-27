//! Protocol-solvency arithmetic — pure, host-tested, byte-for-byte equivalent to
//! the anchor `matcher::insurance::assess_solvency` (which is Kani-proven there).
//!
//! Solvent iff the shared quote vault covers the protocol-owned buckets it backs:
//! the insurance balance plus the FLP capital pool. When solvent the surplus is
//! the exact remainder (`vault - required`) — no value is invented; when
//! insolvent the surplus is 0. Overflow in the (insurance + flp) sum is a hard
//! error rather than a wraparound that could mask insolvency.

/// Returns `(solvent, surplus_quote_lots)`. `Err(())` only on an
/// `insurance + flp_capital` overflow (impossible with real balances, but
/// checked so it can never wrap into a false "solvent").
pub fn assess_solvency(vault: u64, insurance: u64, flp_capital: u64) -> Result<(bool, u64), ()> {
    let minimum_required = insurance.checked_add(flp_capital).ok_or(())?;
    let solvent = vault >= minimum_required;
    let surplus = if solvent { vault - minimum_required } else { 0 };
    Ok((solvent, surplus))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solvent_when_vault_covers_buckets_and_surplus_is_exact() {
        // vault exactly equals required → solvent, zero surplus.
        assert_eq!(assess_solvency(100, 60, 40), Ok((true, 0)));
        // vault above required → solvent, surplus is the remainder.
        assert_eq!(assess_solvency(150, 60, 40), Ok((true, 50)));
        // vault below required → insolvent, surplus floored at 0 (not wrapped).
        assert_eq!(assess_solvency(99, 60, 40), Ok((false, 0)));
    }

    #[test]
    fn zero_buckets_is_trivially_solvent() {
        assert_eq!(assess_solvency(0, 0, 0), Ok((true, 0)));
        assert_eq!(assess_solvency(5, 0, 0), Ok((true, 5)));
    }

    #[test]
    fn bucket_sum_overflow_is_an_error_not_a_wrap() {
        // insurance + flp overflows u64 → Err, never a false solvent.
        assert_eq!(assess_solvency(u64::MAX, u64::MAX, 1), Err(()));
    }

    #[test]
    fn surplus_never_exceeds_vault() {
        for &(v, i, f) in &[(u64::MAX, 0, 0), (1_000_000, 1, 1), (u64::MAX, u64::MAX, 0)] {
            if let Ok((solvent, surplus)) = assess_solvency(v, i, f) {
                assert!(surplus <= v);
                if solvent {
                    assert_eq!(surplus, v - (i + f));
                }
            }
        }
    }
}
