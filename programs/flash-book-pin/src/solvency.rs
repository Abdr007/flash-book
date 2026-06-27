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

/// One-sided insolvency detector — `true` ⇒ the protocol is *definitely*
/// insolvent (anchor `partial_collateral_proves_insolvent`, Kani-proven there).
///
/// `headroom = vault − (flp_capital + insurance)` saturating at 0 is the slice
/// of the vault not already owed to the protocol-owned buckets. If a *partial*
/// sum of trader collateral already exceeds that headroom, the full set must too
/// — so any caller can prove insolvency without summing every trader. `false` is
/// inconclusive (a larger trader set may still prove it). `Err(())` only on a
/// `flp_capital + insurance` overflow.
pub fn partial_collateral_proves_insolvent(
    partial_collateral: u64,
    flp_capital: u64,
    insurance: u64,
    vault: u64,
) -> Result<bool, ()> {
    let buckets = flp_capital.checked_add(insurance).ok_or(())?;
    let headroom = vault.saturating_sub(buckets);
    Ok(partial_collateral > headroom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_proof_is_one_sided_and_monotone() {
        // vault 100, buckets 60 → headroom 40. collateral 41 proves insolvency.
        assert_eq!(partial_collateral_proves_insolvent(41, 30, 30, 100), Ok(true));
        // collateral exactly at headroom is NOT proof (vault still covers).
        assert_eq!(partial_collateral_proves_insolvent(40, 30, 30, 100), Ok(false));
        // buckets alone exceed vault → ANY positive collateral proves it.
        assert_eq!(partial_collateral_proves_insolvent(1, 80, 80, 100), Ok(true));
        assert_eq!(partial_collateral_proves_insolvent(0, 80, 80, 100), Ok(false));
    }

    #[test]
    fn detector_bucket_overflow_is_an_error() {
        assert_eq!(
            partial_collateral_proves_insolvent(1, u64::MAX, 1, u64::MAX),
            Err(())
        );
    }

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
