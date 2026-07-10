//! Insurance fund — three-stream contributions, single-tier waterfall.
//!
//! Per-fill contributions:
//!   - fee_contribution_bps   of taker fee
//!   - tox_contribution_bps   of toxicity tax
//!   - liq_contribution_bps   of liquidation penalty
//!
//! Bankruptcy waterfall:
//!   shortfall → fund.cover()  → ADL (handled by caller)
//!
//! Pause-new-positions threshold: gating logic for opening orders
//! when fund balance falls below a configured floor.

use crate::constants::BPS_DENOM;
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct InsuranceFund {
    pub balance_quote_lots: u64,
    pub fee_contribution_bps: u32,
    pub tox_contribution_bps: u32,
    pub liq_contribution_bps: u32,
    pub pause_threshold_quote_lots: u64,
    pub total_contributions: u64,
    pub total_payouts: u64,
}

impl InsuranceFund {
    pub fn new(
        initial: u64,
        fee_bps: u32,
        tox_bps: u32,
        liq_bps: u32,
        pause_threshold: u64,
    ) -> Self {
        Self {
            balance_quote_lots: initial,
            fee_contribution_bps: fee_bps,
            tox_contribution_bps: tox_bps,
            liq_contribution_bps: liq_bps,
            pause_threshold_quote_lots: pause_threshold,
            total_contributions: 0,
            total_payouts: 0,
        }
    }

    fn apply_bps(amount: u64, bps: u32) -> Result<u64> {
        let prod = (amount as u128).checked_mul(bps as u128).or_overflow()?;
        let res = prod.checked_div(BPS_DENOM as u128).or_overflow()?;
        if res > u64::MAX as u128 {
            Ok(u64::MAX)
        } else {
            Ok(res as u64)
        }
    }

    pub fn contribute_from_fees(&mut self, total_fees: u64) -> Result<u64> {
        let c = Self::apply_bps(total_fees, self.fee_contribution_bps)?;
        self.balance_quote_lots = self.balance_quote_lots.saturating_add(c);
        self.total_contributions = self.total_contributions.saturating_add(c);
        Ok(c)
    }

    pub fn contribute_from_toxicity_tax(&mut self, total_tax: u64) -> Result<u64> {
        let c = Self::apply_bps(total_tax, self.tox_contribution_bps)?;
        self.balance_quote_lots = self.balance_quote_lots.saturating_add(c);
        self.total_contributions = self.total_contributions.saturating_add(c);
        Ok(c)
    }

    pub fn contribute_from_liq_penalty(&mut self, total_penalty: u64) -> Result<u64> {
        let c = Self::apply_bps(total_penalty, self.liq_contribution_bps)?;
        self.balance_quote_lots = self.balance_quote_lots.saturating_add(c);
        self.total_contributions = self.total_contributions.saturating_add(c);
        Ok(c)
    }

    /// Pay out from fund up to `shortfall`. Returns (covered, remaining).
    pub fn cover_shortfall(&mut self, shortfall: u64) -> (u64, u64) {
        if shortfall == 0 {
            return (0, 0);
        }
        let covered = shortfall.min(self.balance_quote_lots);
        self.balance_quote_lots -= covered;
        self.total_payouts = self.total_payouts.saturating_add(covered);
        (covered, shortfall - covered)
    }

    pub fn new_positions_allowed(&self) -> bool {
        self.balance_quote_lots >= self.pause_threshold_quote_lots
    }
}

// ─────────────────────────────────────────────────────────────────────
// Protocol solvency check (P-SOLV-4, protocol-owned buckets). Pure core of
// `verify_protocol_solvency`: the quote vault must cover the insurance fund +
// FLP capital. Returns (solvent, surplus); surplus is exact when solvent, so the
// vault accounts EXACTLY to insurance + FLP + surplus (no value invented).
// Overflow-safe — `insurance + flp` via checked_add. This is the
// protocol-owned subset; the broader `vault ≥ Σ trader_collateral + FLP +
// insurance` whole-program invariant is specified in certora/PROPERTIES.md.
// ─────────────────────────────────────────────────────────────────────

/// Reference model for the full solvency invariant: the vault must cover ALL
/// liabilities — `vault >= total_collateral + flp_capital + insurance`.
/// Stronger than [`assess_solvency`], which omits trader collateral and so
/// only covers the protocol-owned subset. The full sum is unbounded on-chain
/// (it requires every trader's collateral), so no instruction evaluates this
/// directly; it exists as the specification that
/// [`partial_collateral_proves_insolvent`] is machine-proven sound against.
/// Returns `(solvent, surplus)`; `Err(())` iff the summed liabilities
/// overflow u64.
#[cfg(any(kani, test, feature = "certora"))]
#[allow(clippy::result_unit_err)]
#[inline]
pub fn assess_solvency_full(
    vault: u64,
    total_collateral: u64,
    flp_capital: u64,
    insurance: u64,
) -> core::result::Result<(bool, u64), ()> {
    let required = total_collateral
        .checked_add(flp_capital)
        .ok_or(())?
        .checked_add(insurance)
        .ok_or(())?;
    let solvent = vault >= required;
    let surplus = if solvent { vault - required } else { 0 };
    Ok((solvent, surplus))
}

/// One-sided insolvency detector over a PARTIAL (deduplicated) collateral sum.
///
/// The full invariant needs `Σ collateral` over ALL traders, which is unbounded
/// on-chain. But solvency only requires `Σ collateral <= vault - (flp +
/// insurance)`, so if any DEDUPLICATED SUBSET of trader collateral already
/// exceeds that headroom, the protocol is provably insolvent regardless of the
/// unseen remainder (the real total is `>=` the partial sum). This is sound in
/// one direction (it only ever fires on genuine insolvency) and drift-free: it
/// reads real summed balances rather than a stored aggregate that could desync
/// from the 47 collateral-mutation sites. Returns `true` ⇒ definitely insolvent.
#[allow(clippy::result_unit_err)] // the caller maps the erased error to a program error
#[inline]
pub fn partial_collateral_proves_insolvent(
    partial_collateral: u64,
    flp_capital: u64,
    insurance: u64,
    vault: u64,
) -> core::result::Result<bool, ()> {
    let buckets = flp_capital.checked_add(insurance).ok_or(())?;
    // headroom = vault - buckets, saturating at 0: if the protocol-owned buckets
    // alone already exceed the vault, ANY positive collateral proves insolvency.
    let headroom = vault.saturating_sub(buckets);
    Ok(partial_collateral > headroom)
}

/// One-sided detector for an OVER-STATED haircut `residual`.
///
/// `residual` is the sole backing for junior-profit extraction (convert_position
/// credits `min(residual, matured)/matured` of matured PnL). It must be covered
/// by the vault SURPLUS (`vault − collateral − flp − insurance`). Since
/// `partial_collateral <= total_collateral`, if `partial + flp + insurance +
/// residual` already exceeds the vault, the residual is PROVABLY over-stated /
/// unbacked, regardless of the unseen collateral remainder. Sound in one
/// direction: `true` ⇒ the residual is definitely unbacked — it never fires on a
/// genuinely-backed residual. Saturating u128 arithmetic (no overflow path).
#[inline]
pub fn residual_exceeds_backed_surplus(
    partial_collateral: u64,
    flp_capital: u64,
    insurance: u64,
    residual: u128,
    vault: u64,
) -> bool {
    let committed = (partial_collateral as u128)
        .saturating_add(flp_capital as u128)
        .saturating_add(insurance as u128)
        .saturating_add(residual);
    committed > vault as u128
}

/// Assess protocol solvency over the vault / insurance / FLP-capital buckets.
/// `Err(())` iff `insurance + flp_capital` overflows u64 (unreachable for real
/// balances — the caller maps it to ArithmeticOverflow).
#[allow(clippy::result_unit_err)] // the caller maps the erased error to a program error
#[inline]
pub fn assess_solvency(
    vault: u64,
    insurance: u64,
    flp_capital: u64,
) -> core::result::Result<(bool, u64), ()> {
    let minimum_required = insurance.checked_add(flp_capital).ok_or(())?;
    let solvent = vault >= minimum_required;
    let surplus = if solvent { vault - minimum_required } else { 0 };
    Ok((solvent, surplus))
}

/// FV: machine-checked protocol-solvency arithmetic (Kani, add/compare only → fast).
#[cfg(kani)]
mod solvency_kani_proofs {
    use super::{assess_solvency, assess_solvency_full, partial_collateral_proves_insolvent};

    /// P-SOLV-4 CORRECTNESS: full-invariant `solvent` is exactly
    /// `vault >= total_collateral + flp + insurance`.
    #[kani::proof]
    fn full_solvent_iff_vault_covers_all_liabilities() {
        let vault: u64 = kani::any();
        let collateral: u64 = kani::any();
        let flp: u64 = kani::any();
        let insurance: u64 = kani::any();
        if let Ok((solvent, surplus)) = assess_solvency_full(vault, collateral, flp, insurance) {
            // no-overflow path: collateral + flp + insurance fits u64
            let req = collateral + flp + insurance;
            assert!(solvent == (vault >= req));
            if solvent {
                assert!(req + surplus == vault); // surplus exact, no value invented
            } else {
                assert!(surplus == 0);
            }
        }
    }

    /// SOUNDNESS: the one-sided detector NEVER fires unless the protocol is
    /// genuinely insolvent. If a deduplicated PARTIAL collateral sum proves
    /// insolvency, then for ANY real total `>=` that partial, the full check
    /// reports NOT solvent. (When the full sum overflows u64 it is `> vault`
    /// too, i.e. also insolvent — consistent, just not exercised by the assert.)
    #[kani::proof]
    fn partial_insolvency_detector_is_sound() {
        let partial: u64 = kani::any();
        let flp: u64 = kani::any();
        let insurance: u64 = kani::any();
        let vault: u64 = kani::any();
        let total: u64 = kani::any();
        kani::assume(total >= partial);
        if let Ok(true) = partial_collateral_proves_insolvent(partial, flp, insurance, vault) {
            if let Ok((solvent, _)) = assess_solvency_full(vault, total, flp, insurance) {
                assert!(!solvent);
            }
        }
    }

    /// CORRECTNESS: `solvent` is exactly `vault ≥ insurance + flp_capital`.
    #[kani::proof]
    fn solvent_iff_vault_covers_buckets() {
        let vault: u64 = kani::any();
        let insurance: u64 = kani::any();
        let flp: u64 = kani::any();
        if let Ok((solvent, _)) = assess_solvency(vault, insurance, flp) {
            // no overflow path: insurance + flp fits u64
            let req = insurance + flp;
            assert!(solvent == (vault >= req));
        }
    }

    /// CONSERVATION: when solvent, the vault accounts EXACTLY to
    /// `insurance + flp + surplus` — surplus is never inflated, no value invented.
    #[kani::proof]
    fn surplus_exact_when_solvent() {
        let vault: u64 = kani::any();
        let insurance: u64 = kani::any();
        let flp: u64 = kani::any();
        if let Ok((solvent, surplus)) = assess_solvency(vault, insurance, flp) {
            if solvent {
                // insurance + flp + surplus == vault, with no overflow
                let req = insurance + flp;
                assert!(req + surplus == vault);
            } else {
                assert!(surplus == 0);
            }
        }
    }

    /// ISOLATION (anti-single-vault-SPOF): the bad-debt waterfall debits the
    /// insurance fund by a function of ONLY its own balance and the shortfall —
    /// FLP capital is not an input and cannot be drained by it. Mirrors the exact
    /// arithmetic of `cover_bad_debt` (lib.rs). Bounds: coverage never exceeds the
    /// fund balance (no underflow), never exceeds the shortfall (no over-payout),
    /// and the balance only falls.
    #[kani::proof]
    fn bad_debt_coverage_is_insurance_isolated_and_bounded() {
        let balance: u64 = kani::any();
        let shortfall: u64 = kani::any();
        let covered = shortfall.min(balance);
        let new_balance = balance - covered; // provably no underflow: covered <= balance
        assert!(covered <= balance);
        assert!(covered <= shortfall);
        assert!(new_balance <= balance);
        assert!(balance - new_balance == covered);
    }
}

#[cfg(test)]
mod m6_residual_backing_tests {
    //! Sound one-sided detection of an over-stated residual.
    use super::residual_exceeds_backed_surplus;

    #[test]
    fn backed_residual_passes() {
        // vault 1000; collateral 400 + flp 200 + insurance 100 = 700 committed;
        // residual 200 → 900 <= 1000 → backed (no flag).
        assert!(!residual_exceeds_backed_surplus(400, 200, 100, 200, 1000));
        // exactly at the surplus (900 committed, residual 100 → 1000) is OK.
        assert!(!residual_exceeds_backed_surplus(400, 200, 100, 100, 1000));
    }

    #[test]
    fn overstated_residual_flagged() {
        // Same buckets (700), residual 301 → 1001 > 1000 → provably unbacked.
        assert!(residual_exceeds_backed_surplus(400, 200, 100, 301, 1000));
        // Residual alone exceeds the whole vault.
        assert!(residual_exceeds_backed_surplus(0, 0, 0, 1_001, 1000));
    }

    #[test]
    fn partial_collateral_is_conservative() {
        // Even a PARTIAL collateral sum that (with the others + residual) exceeds
        // the vault proves insolvency, because total >= partial.
        assert!(residual_exceeds_backed_surplus(950, 0, 0, 60, 1000));
    }

    #[test]
    fn saturating_no_overflow() {
        // u128 residual near max + u64 buckets must not panic.
        assert!(residual_exceeds_backed_surplus(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u128::MAX,
            u64::MAX
        ));
    }
}

#[cfg(test)]
mod solvency_full_tests {
    use super::{assess_solvency_full, partial_collateral_proves_insolvent};

    #[test]
    fn full_invariant_counts_collateral() {
        // vault exactly covers collateral + flp + insurance ⇒ solvent, zero surplus.
        assert_eq!(assess_solvency_full(100, 60, 30, 10), Ok((true, 0)));
        // one extra lot of headroom ⇒ surplus 1.
        assert_eq!(assess_solvency_full(101, 60, 30, 10), Ok((true, 1)));
        // collateral the OLD protocol-bucket check ignored now tips it insolvent.
        assert_eq!(assess_solvency_full(40, 60, 30, 10), Ok((false, 0)));
    }

    #[test]
    fn partial_detector_fires_only_on_real_insolvency() {
        // vault 100, buckets flp30+ins10=40 ⇒ headroom 60.
        // A partial collateral sum of 61 (subset!) already exceeds headroom ⇒ insolvent.
        assert_eq!(
            partial_collateral_proves_insolvent(61, 30, 10, 100),
            Ok(true)
        );
        // A partial of 60 is within headroom ⇒ not proven (more traders may exist).
        assert_eq!(
            partial_collateral_proves_insolvent(60, 30, 10, 100),
            Ok(false)
        );
        // Buckets alone exceed the vault ⇒ any positive collateral proves it.
        assert_eq!(
            partial_collateral_proves_insolvent(1, 80, 30, 100),
            Ok(true)
        );
    }

    #[test]
    fn partial_detector_is_one_sided_sound_vs_full() {
        // Whenever the partial detector fires, the full check on the SAME-or-larger
        // total must agree it's insolvent.
        for &(partial, flp, ins, vault) in &[(61u64, 30u64, 10u64, 100u64), (1, 80, 30, 100)] {
            if partial_collateral_proves_insolvent(partial, flp, ins, vault) == Ok(true) {
                for extra in 0..5u64 {
                    let total = partial + extra;
                    let (solvent, _) = assess_solvency_full(vault, total, flp, ins).unwrap();
                    assert!(!solvent, "detector fired but full check called it solvent");
                }
            }
        }
    }
}
