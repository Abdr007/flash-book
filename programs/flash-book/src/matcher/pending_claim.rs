//! Pending claim primitive (Wave 36).
//!
//! When a trader's position close can't be paid out of the pool (the
//! pool ran out of the asset the trader is owed in), we credit a
//! `PendingClaim` instead of reverting or auto-deleveraging. The
//! trader redeems the claim later when liquidity returns.
//!
//! Adapted from GMX V2's `claimableCollateralAmount` mechanism — soft-
//! fail UX instead of hard-fail or ADL.
//!
//! Pure math. The on-chain state lives in a new PDA
//! `PendingClaimAccount` keyed `[b"pending_claim", trader, mint]`.

/// Pure: compute the actual payout + remaining claim given a desired
/// payout, the pool's available balance, and the existing claim.
///
/// Returns `(actual_payout, new_total_claim)`. If the pool can fully
/// pay, `actual_payout == desired` and the claim stays at the existing
/// level. If not, `actual_payout == available` and the unpaid portion
/// rolls into the claim.
pub fn split_payout_and_claim(
    desired_payout: u64,
    pool_available: u64,
    existing_claim: u64,
) -> (u64, u64) {
    if desired_payout == 0 {
        return (0, existing_claim);
    }
    let payout = desired_payout.min(pool_available);
    let shortfall = desired_payout.saturating_sub(payout);
    let new_claim = existing_claim.saturating_add(shortfall);
    (payout, new_claim)
}

/// Redeem an existing claim against current pool liquidity.
///
/// Returns `(amount_paid, remaining_claim)`.
pub fn redeem_claim(claim: u64, pool_available: u64) -> (u64, u64) {
    let pay = claim.min(pool_available);
    let rem = claim.saturating_sub(pay);
    (pay, rem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_payout_when_pool_has_liquidity() {
        assert_eq!(split_payout_and_claim(100, 200, 0), (100, 0));
    }

    #[test]
    fn shortfall_rolls_into_claim() {
        assert_eq!(split_payout_and_claim(100, 60, 0), (60, 40));
    }

    #[test]
    fn shortfall_accumulates_on_top_of_existing_claim() {
        assert_eq!(split_payout_and_claim(100, 60, 25), (60, 65));
    }

    #[test]
    fn zero_payout_is_noop() {
        assert_eq!(split_payout_and_claim(0, 200, 50), (0, 50));
    }

    #[test]
    fn redeem_full_claim_when_pool_has_it() {
        assert_eq!(redeem_claim(100, 200), (100, 0));
    }

    #[test]
    fn redeem_partial_when_pool_insufficient() {
        assert_eq!(redeem_claim(100, 40), (40, 60));
    }

    #[test]
    fn redeem_zero_when_no_liquidity() {
        assert_eq!(redeem_claim(100, 0), (0, 100));
    }

    #[test]
    fn redeem_zero_claim_is_noop() {
        assert_eq!(redeem_claim(0, 200), (0, 0));
    }
}
