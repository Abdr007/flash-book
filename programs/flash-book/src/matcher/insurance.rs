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
// Overflow-safe — `insurance + flp` via checked_add. NOTE: this is the
// protocol-owned subset; the broader `vault ≥ Σ trader_collateral + FLP +
// insurance` is the [CERTORA-TARGET] whole-program invariant.
// ─────────────────────────────────────────────────────────────────────

/// Assess protocol solvency over the vault / insurance / FLP-capital buckets.
/// `Err(())` iff `insurance + flp_capital` overflows u64 (unreachable for real
/// balances — the caller maps it to ArithmeticOverflow).
#[inline]
pub fn assess_solvency(
    vault: u64,
    insurance: u64,
    flp_capital: u64,
) -> core::result::Result<(bool, u64), ()> {
    let minimum_required = insurance.checked_add(flp_capital).ok_or(())?;
    let solvent = vault >= minimum_required;
    let surplus = if solvent {
        vault - minimum_required
    } else {
        0
    };
    Ok((solvent, surplus))
}

/// FV: machine-checked protocol-solvency arithmetic (Kani, add/compare only → fast).
#[cfg(kani)]
mod solvency_kani_proofs {
    use super::assess_solvency;

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
}
