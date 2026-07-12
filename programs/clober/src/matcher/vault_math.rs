//! Copy-vault share accounting (ERC-4626-style), the safety-critical core of the
//! on-chain copy/managed vault. Depositors receive SHARES of the vault; the
//! vault's manager trades its pooled collateral; each depositor's claim is
//! `shares / total_shares` of the vault's accounted assets (its trader-state
//! collateral in quote lots).
//!
//! `assets` here is the vault's ACCOUNTED collateral — a field the program
//! controls, changed only by deposit / withdraw / settled trade PnL — NOT a raw
//! token-account balance. Because a donation to the vault's ATA cannot change the
//! accounted assets, the classic ERC-4626 "first-depositor inflation / donation"
//! attack does not apply, so the plain proportional formula is sound.
//!
//! All arithmetic is integer, checked/saturating. The proportional-share
//! theorems (no dilution, round-trip ≤ deposit) are machine-checked at unbounded
//! width in `formal_verification/lean/VaultShares.lean` — CBMC cannot discharge
//! the symbolic `× / total` division, so Lean is the durable proof; the unit
//! tests below pin concrete cases.

use anchor_lang::prelude::*;

use crate::errors::CloberError;

/// Shares minted for a `deposit` into a vault holding `total_assets` backed by
/// `total_shares`. First deposit (`total_shares == 0`) seeds 1:1. Otherwise
/// `shares = floor(deposit × total_shares / total_assets)` — rounds DOWN, so
/// rounding always favours the existing holders (a depositor never mints more
/// than their exact proportional claim). `total_assets == 0` with
/// `total_shares > 0` is a drained vault: reject (a deposit then couldn't be
/// priced), surfaced as `DivisionByZero`.
#[inline]
pub fn shares_on_deposit(deposit: u64, total_shares: u64, total_assets: u64) -> Result<u64> {
    if total_shares == 0 {
        return Ok(deposit); // seed 1:1
    }
    require!(total_assets > 0, CloberError::DivisionByZero);
    let s = (deposit as u128)
        .checked_mul(total_shares as u128)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))?
        / (total_assets as u128);
    if s > u64::MAX as u128 {
        return Err(error!(CloberError::ArithmeticOverflow));
    }
    Ok(s as u64)
}

/// Assets returned for burning `shares` of a vault holding `total_assets` backed
/// by `total_shares`. `assets = floor(shares × total_assets / total_shares)` —
/// rounds DOWN, so the vault never pays out more than the burned proportion.
/// Requires `shares ≤ total_shares` (can't burn more than exist) and
/// `total_shares > 0`.
#[inline]
pub fn assets_on_withdraw(shares: u64, total_shares: u64, total_assets: u64) -> Result<u64> {
    require!(total_shares > 0, CloberError::DivisionByZero);
    require!(shares <= total_shares, CloberError::OutOfRange);
    let a = (shares as u128)
        .checked_mul(total_assets as u128)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))?
        / (total_shares as u128);
    // a ≤ total_assets because shares ≤ total_shares ⇒ floor(shares·A/total) ≤ A.
    Ok(a as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_seeds_one_to_one() {
        assert_eq!(shares_on_deposit(1_000, 0, 0).unwrap(), 1_000);
        assert_eq!(shares_on_deposit(1_000, 0, 999).unwrap(), 1_000); // assets ignored when no shares
    }

    #[test]
    fn proportional_deposit() {
        // vault: 1_000 shares / 1_000 assets (price 1.0). deposit 500 → 500 shares.
        assert_eq!(shares_on_deposit(500, 1_000, 1_000).unwrap(), 500);
        // vault appreciated: 1_000 shares / 2_000 assets (price 2.0). deposit 500 → 250 shares.
        assert_eq!(shares_on_deposit(500, 1_000, 2_000).unwrap(), 250);
    }

    #[test]
    fn withdraw_is_proportional_and_bounded() {
        // burn 250 of 1_000 shares against 2_000 assets → 500 assets.
        assert_eq!(assets_on_withdraw(250, 1_000, 2_000).unwrap(), 500);
        // burning ALL shares returns ALL assets.
        assert_eq!(assets_on_withdraw(1_000, 1_000, 2_000).unwrap(), 2_000);
        // never more than the pool.
        assert!(assets_on_withdraw(1, 1_000, 2_000).unwrap() <= 2_000);
    }

    #[test]
    fn deposit_then_withdraw_never_extracts_value() {
        // Round-trip: deposit d, then immediately burn the minted shares.
        // Rounding-down at both ends ⇒ you never get back MORE than you put in.
        for &(d, ts, ta) in &[
            (500u64, 1_000u64, 2_000u64),
            (333, 777, 1_001),
            (1, 1_000_000, 1),
        ] {
            let minted = shares_on_deposit(d, ts, ta).unwrap();
            let ta2 = ta + d; // vault assets grew by the deposit
            let ts2 = ts + minted;
            let back = assets_on_withdraw(minted, ts2, ta2).unwrap();
            assert!(back <= d, "round-trip extracted value: d={d} back={back}");
        }
    }

    #[test]
    fn drained_vault_deposit_rejected() {
        // total_shares > 0 but total_assets == 0 (fully drained) ⇒ reject.
        assert!(shares_on_deposit(100, 1_000, 0).is_err());
    }

    #[test]
    fn cannot_burn_more_shares_than_exist() {
        assert!(assets_on_withdraw(1_001, 1_000, 2_000).is_err());
    }
}
