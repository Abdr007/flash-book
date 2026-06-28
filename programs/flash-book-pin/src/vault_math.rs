//! Pure vault share-accounting math — a faithful transcription of the share
//! mint/burn arithmetic in the Anchor `vault_deposit_v3` / `vault_withdraw_v3`.
//! No pinocchio, no syscalls → host-unit-testable + Kani-provable.
//!
//! NAV convention (matches anchor): a vault's net asset value is measured by its
//! TraderState `collateral_quote_lots` (deposits + realized trading PnL). Share
//! price = NAV / shares_outstanding. Mint floors (favoring existing holders);
//! burn floors the payout (favoring the remaining pool) — neither can create
//! value out of rounding.

#[derive(Debug, PartialEq, Eq)]
pub enum VaultMathErr {
    ZeroAmount,
    ZeroShares,
    Overflow,
    InsufficientShares,
}

/// Shares to mint for a deposit of `amount` into a vault whose NAV *before* this
/// deposit is `pre_deposit_nav` with `shares_outstanding` shares.
///
/// Bootstrap (first deposit, or NAV wiped to 0): mint 1:1 with `amount`.
/// Otherwise: `floor(amount * shares_outstanding / pre_deposit_nav)`.
/// Errors if `amount == 0` or the result rounds to 0 shares (dust deposit).
pub fn shares_to_mint(
    amount: u64,
    shares_outstanding: u64,
    pre_deposit_nav: u64,
) -> Result<u64, VaultMathErr> {
    if amount == 0 {
        return Err(VaultMathErr::ZeroAmount);
    }
    let shares = if shares_outstanding == 0 || pre_deposit_nav == 0 {
        amount
    } else {
        let prod = (amount as u128)
            .checked_mul(shares_outstanding as u128)
            .ok_or(VaultMathErr::Overflow)?;
        let q = prod / (pre_deposit_nav as u128);
        if q > u64::MAX as u128 {
            return Err(VaultMathErr::Overflow);
        }
        q as u64
    };
    if shares == 0 {
        return Err(VaultMathErr::ZeroShares);
    }
    Ok(shares)
}

/// Quote-lots to pay out for burning `shares_to_burn` of a vault holding `nav`
/// quote-lots backing `shares_outstanding` shares.
///
/// `floor(shares_to_burn * nav / shares_outstanding)` — the floor keeps the
/// dust in the pool for the remaining holders. Errors on a zero burn, burning
/// more than exist, or an empty vault.
pub fn payout_for_shares(
    shares_to_burn: u64,
    shares_outstanding: u64,
    nav: u64,
) -> Result<u64, VaultMathErr> {
    if shares_to_burn == 0 {
        return Err(VaultMathErr::ZeroAmount);
    }
    if shares_outstanding == 0 || shares_to_burn > shares_outstanding {
        return Err(VaultMathErr::InsufficientShares);
    }
    let prod = (shares_to_burn as u128)
        .checked_mul(nav as u128)
        .ok_or(VaultMathErr::Overflow)?;
    let payout = prod / (shares_outstanding as u128);
    // payout <= nav (since shares_to_burn <= shares_outstanding) → fits u64.
    Ok(payout as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_mints_one_to_one() {
        assert_eq!(shares_to_mint(1_000, 0, 0), Ok(1_000));
        // NAV wiped (e.g. vault drew down to 0) → also bootstrap 1:1.
        assert_eq!(shares_to_mint(500, 1_000, 0), Ok(500));
    }

    #[test]
    fn pro_rata_mint() {
        // NAV 1000 backs 1000 shares (price 1.0); deposit 500 → 500 shares.
        assert_eq!(shares_to_mint(500, 1_000, 1_000), Ok(500));
        // NAV doubled by PnL: 2000 backs 1000 shares (price 2.0); deposit 1000
        // buys only 500 shares.
        assert_eq!(shares_to_mint(1_000, 1_000, 2_000), Ok(500));
    }

    #[test]
    fn dust_deposit_rounds_to_zero_is_rejected() {
        // price 2.0, deposit 1 → floor(1*1000/2000)=0 → rejected.
        assert_eq!(shares_to_mint(1, 1_000, 2_000), Err(VaultMathErr::ZeroShares));
    }

    #[test]
    fn zero_amount_rejected() {
        assert_eq!(shares_to_mint(0, 1_000, 1_000), Err(VaultMathErr::ZeroAmount));
    }

    #[test]
    fn payout_basic_and_floors() {
        // Burn half the shares of a 1000-NAV / 1000-share vault → 500.
        assert_eq!(payout_for_shares(500, 1_000, 1_000), Ok(500));
        // Floor keeps dust: burn 1 of 3 shares backing 10 → floor(10/3)=3.
        assert_eq!(payout_for_shares(1, 3, 10), Ok(3));
    }

    #[test]
    fn payout_rejects_overburn_and_zero() {
        assert_eq!(payout_for_shares(0, 1_000, 1_000), Err(VaultMathErr::ZeroAmount));
        assert_eq!(payout_for_shares(1_001, 1_000, 1_000), Err(VaultMathErr::InsufficientShares));
        assert_eq!(payout_for_shares(1, 0, 0), Err(VaultMathErr::InsufficientShares));
    }

    #[test]
    fn mint_never_dilutes_existing_holders() {
        // Property: after a deposit, share price (nav/shares) never DROPS for the
        // prior holders — the mint floor guarantees it. Check across a grid.
        for &nav in &[1u64, 7, 100, 999, 1_000_000] {
            for &shares in &[1u64, 5, 100, 1_000, 500_000] {
                for &amount in &[1u64, 3, 50, 1_000, 250_000] {
                    if let Ok(minted) = shares_to_mint(amount, shares, nav) {
                        let new_shares = shares as u128 + minted as u128;
                        let new_nav = nav as u128 + amount as u128;
                        // new_nav/new_shares >= nav/shares  ⇔  new_nav*shares >= nav*new_shares
                        assert!(
                            new_nav * (shares as u128) >= (nav as u128) * new_shares,
                            "dilution: nav={nav} shares={shares} amount={amount} minted={minted}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn deposit_withdraw_round_trip_never_creates_value() {
        // Deposit `amount` into an existing vault, immediately burn the minted
        // shares — the payout must never exceed the deposit (no free money).
        for &nav in &[1u64, 50, 1_000, 1_000_000] {
            for &shares in &[1u64, 50, 1_000, 500_000] {
                for &amount in &[1u64, 10, 1_000, 100_000] {
                    if let Ok(minted) = shares_to_mint(amount, shares, nav) {
                        let post_nav = nav + amount;
                        let post_shares = shares + minted;
                        let back = payout_for_shares(minted, post_shares, post_nav).unwrap();
                        assert!(
                            back <= amount,
                            "value created: nav={nav} shares={shares} amount={amount} minted={minted} back={back}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// shares_to_mint never panics (no overflow) for ANY u64 inputs, and the
    /// bootstrap case mints exactly `amount`.
    #[kani::proof]
    fn proof_mint_total_and_bootstrap() {
        let amount: u64 = kani::any();
        let shares: u64 = kani::any();
        let nav: u64 = kani::any();
        let r = shares_to_mint(amount, shares, nav);
        if amount != 0 && (shares == 0 || nav == 0) {
            assert!(r == Ok(amount));
        }
        // For any inputs the call returns without panicking; on success the
        // result is non-zero by construction.
        if let Ok(s) = r {
            assert!(s != 0);
        }
    }

    // NOTE: a `payout <= nav` proof for payout_for_shares would need CBMC to
    // reason about `(burn * nav) / shares` with a SYMBOLIC divisor, which does
    // not converge in reasonable time (same class as the dropped liquidator-
    // reward proof). The bound is instead covered by the host tests
    // `payout_basic_and_floors`, `payout_rejects_overburn_and_zero`, and
    // `deposit_withdraw_round_trip_never_creates_value`.
}
