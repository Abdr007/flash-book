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
    /// Shares are outstanding but NAV is 0 (vault realized to nothing) — the pool
    /// is insolvent and can't be priced; a 1:1 deposit would dilute the depositor
    /// into worthless legacy shares. Matches the FLP twin's `None`.
    Insolvent,
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
    let shares = if shares_outstanding == 0 {
        amount // genuine first deposit ⇒ 1:1 bootstrap
    } else if pre_deposit_nav == 0 {
        // Shares outstanding but NAV 0 ⇒ insolvent; reject (was: minted 1:1,
        // letting the deposit's value accrue to the dead legacy shares).
        return Err(VaultMathErr::Insolvent);
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

use crate::constants::{BPS_DENOM, USD_UNIT};

/// Current NAV per share in 1e6 fixed point: `floor(nav * USD_UNIT / shares)`.
/// Zero shares → 0 (caller bootstraps the HWM separately).
pub fn nav_per_share_x6(nav: u64, shares_outstanding: u64) -> u64 {
    if shares_outstanding == 0 {
        return 0;
    }
    let x = (nav as u128).saturating_mul(USD_UNIT as u128) / (shares_outstanding as u128);
    if x > u64::MAX as u128 {
        u64::MAX
    } else {
        x as u64
    }
}

/// Performance-fee shares to mint to the strategist when NAV/share rises above
/// the high-water mark. Returns 0 when there is no new high, the vault is empty,
/// the HWM is unset, or the fee rounds to dust — faithful to the Anchor
/// `settle_vault_perf_fee_v3` arithmetic.
///
/// fee_qlots = (nav/share − hwm)/USD_UNIT * shares * perf_fee_bps/BPS_DENOM;
/// minted = fee_qlots * shares / nav (dilution that prices the fee in shares).
pub fn perf_fee_shares(
    nav: u64,
    shares_outstanding: u64,
    nav_per_share: u64,
    prev_hwm_x6: u64,
    perf_fee_bps: u32,
) -> u64 {
    if shares_outstanding == 0 || nav == 0 || prev_hwm_x6 == 0 || nav_per_share <= prev_hwm_x6 {
        return 0;
    }
    let gain_per_share = (nav_per_share - prev_hwm_x6) as u128;
    let total_gain = gain_per_share.saturating_mul(shares_outstanding as u128) / (USD_UNIT as u128);
    let fee_qlots = total_gain.saturating_mul(perf_fee_bps as u128) / (BPS_DENOM as u128);
    let minted = fee_qlots.saturating_mul(shares_outstanding as u128) / (nav as u128);
    if minted > u64::MAX as u128 {
        u64::MAX
    } else {
        minted as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_mints_one_to_one() {
        assert_eq!(shares_to_mint(1_000, 0, 0), Ok(1_000));
        // Shares outstanding but NAV wiped to 0 ⇒ insolvent, REJECTED (was: minted
        // 1:1, diluting the depositor into the dead legacy shares).
        assert_eq!(shares_to_mint(500, 1_000, 0), Err(VaultMathErr::Insolvent));
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

    #[test]
    fn nav_per_share_basic() {
        // 1000 quote-lots over 1000 shares = 1.0 (1e6 fixed point).
        assert_eq!(nav_per_share_x6(1_000, 1_000), 1_000_000);
        // 2000 over 1000 = 2.0.
        assert_eq!(nav_per_share_x6(2_000, 1_000), 2_000_000);
        // empty vault → 0.
        assert_eq!(nav_per_share_x6(1_000, 0), 0);
    }

    #[test]
    fn perf_fee_no_fee_below_or_at_hwm() {
        // nav/share 1.0, hwm 1.0 → no new high → 0.
        assert_eq!(perf_fee_shares(1_000, 1_000, 1_000_000, 1_000_000, 2_000), 0);
        // nav/share 0.9 < hwm 1.0 → 0.
        assert_eq!(perf_fee_shares(900, 1_000, 900_000, 1_000_000, 2_000), 0);
        // unset hwm → 0 (caller bootstraps).
        assert_eq!(perf_fee_shares(2_000, 1_000, 2_000_000, 0, 2_000), 0);
    }

    #[test]
    fn perf_fee_charges_on_new_high() {
        // NAV doubled: 2000 over 1000 shares → nav/share 2.0, hwm was 1.0.
        // gain/share = 1.0; total_gain = 1.0 * 1000 = 1000 qlots; 20% fee = 200
        // qlots; minted = 200 * 1000 / 2000 = 100 shares.
        let nps = nav_per_share_x6(2_000, 1_000);
        assert_eq!(nps, 2_000_000);
        assert_eq!(perf_fee_shares(2_000, 1_000, nps, 1_000_000, 2_000), 100);
    }

    #[test]
    fn perf_fee_zero_bps_charges_nothing() {
        let nps = nav_per_share_x6(2_000, 1_000);
        assert_eq!(perf_fee_shares(2_000, 1_000, nps, 1_000_000, 0), 0);
    }

    #[test]
    fn perf_fee_minting_does_not_exceed_the_gain_value() {
        // The strategist's minted shares, valued at post-mint NAV/share, must not
        // exceed perf_fee_bps of the gain — i.e. the fee can't over-charge.
        for &nav in &[1_000u64, 50_000, 5_000_000] {
            for &shares in &[1_000u64, 100_000] {
                for &hwm in &[500_000u64, 1_000_000] {
                    let nps = nav_per_share_x6(nav, shares);
                    let minted = perf_fee_shares(nav, shares, nps, hwm, 2_000);
                    if minted > 0 && nps > hwm {
                        // fee value = minted * nav / (shares + minted) <= 20% of gain
                        let gain_qlots = ((nps - hwm) as u128) * (shares as u128) / (USD_UNIT as u128);
                        let max_fee = gain_qlots * 2_000 / (BPS_DENOM as u128);
                        let fee_value = (minted as u128) * (nav as u128) / ((shares + minted) as u128);
                        assert!(fee_value <= max_fee + 1, "over-charge: nav={nav} shares={shares} hwm={hwm} minted={minted}");
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
        if amount != 0 && shares == 0 {
            assert!(r == Ok(amount)); // genuine first deposit bootstraps 1:1
        }
        if amount != 0 && shares != 0 && nav == 0 {
            assert!(r == Err(VaultMathErr::Insolvent)); // insolvent pool rejected
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

    /// perf_fee_shares never panics for ANY u64/u32 inputs, and charges NOTHING
    /// unless NAV/share strictly exceeds the high-water mark (no fee on a flat
    /// or losing vault). The all-saturating arithmetic guarantees no overflow.
    #[kani::proof]
    fn proof_perf_fee_no_charge_below_hwm() {
        let nav: u64 = kani::any();
        let shares: u64 = kani::any();
        let nps: u64 = kani::any();
        let hwm: u64 = kani::any();
        let bps: u32 = kani::any();
        let minted = perf_fee_shares(nav, shares, nps, hwm, bps);
        if nps <= hwm {
            assert!(minted == 0);
        }
    }
}
