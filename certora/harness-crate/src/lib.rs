//! Certora Solana Prover harness for Flash Book P-SOLV-4 (global solvency).
//!
//! Separate verification-only crate (see Cargo.toml for why it is not a module
//! of flash-book). Every rule establishes its pre-state via the REAL,
//! Kani-proven solvency cores in `flash_book::matcher::insurance`, executes a
//! balance-mutating effect with fully symbolic (`nondet`) inputs, and asserts
//! the real invariant still holds — the all-paths obligation Kani cannot give.
//!
//! P-SOLV-4 invariant:  vault ≥ Σ collateral + Σ flp_capital + insurance,
//! computed by the REAL `assess_solvency_full`.
//!
//! COVERAGE: four rules over the REAL cores — withdraw-preserves-solvency,
//! deposit-preserves-solvency, surplus-exactness, and insolvency-detector
//! soundness. These cover the money-movement DIRECTIONS and the runtime
//! solvency detector on real symbols. Per-Anchor-handler dispatch (calling e.g.
//! `check_simple_withdraw` directly) is blocked by a Prover pointer-analysis
//! limit on Anchor's `error!` global-string construction — tracked separately;
//! not counted here.

use cvlr::prelude::*;
use flash_book::matcher::insurance::{assess_solvency_full, partial_collateral_proves_insolvent};

/// Solvent per the REAL invariant function `assess_solvency_full` — NOT an
/// inline re-derivation. This is the exact P-SOLV-4 predicate the on-chain
/// `verify_collateral_solvency` sweep enforces.
fn solvent(vault: u64, total_collateral: u64, flp_capital: u64, insurance: u64) -> bool {
    matches!(
        assess_solvency_full(vault, total_collateral, flp_capital, insurance),
        Ok((true, _))
    )
}

/// P-SOLV-4, withdraw path: solvent + a withdrawal (the same `amount` leaves the
/// vault AND the trader's collateral ledger) ⇒ still solvent, for ALL `u64`.
/// (`amount <= total_collateral` is the numeric core of the real on-chain gate
/// `xmargin::check_simple_withdraw`.)
#[rule]
pub fn solvency_preserved_simple_withdraw() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let flp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let amount: u64 = nondet();

    cvlr_assume!(solvent(vault, total_collateral, flp_capital, insurance));
    cvlr_assume!(amount <= total_collateral);

    // Solvency ⇒ vault ≥ total_collateral ≥ amount, so neither subtraction underflows.
    let vault_post = vault - amount;
    let total_collateral_post = total_collateral - amount;

    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        flp_capital,
        insurance
    ));
}

/// P-SOLV-4, deposit path: solvent + a deposit (the same `amount` enters the
/// vault AND the trader's collateral ledger) ⇒ still solvent, for ALL `u64`
/// that don't overflow. Deposits raise vault and liabilities equally, so the
/// surplus is preserved.
#[rule]
pub fn solvency_preserved_deposit() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let flp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let amount: u64 = nondet();

    cvlr_assume!(solvent(vault, total_collateral, flp_capital, insurance));
    // Real deposits cannot overflow the u64 vault / ledger (checked on-chain).
    cvlr_assume!(vault.checked_add(amount).is_some());
    cvlr_assume!(total_collateral.checked_add(amount).is_some());

    let vault_post = vault + amount;
    let total_collateral_post = total_collateral + amount;

    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        flp_capital,
        insurance
    ));
}

/// Surplus exactness: when solvent, the surplus the REAL `assess_solvency_full`
/// returns is EXACTLY `vault − (collateral + flp + insurance)` — no value is
/// invented or destroyed. Lifts the Kani `surplus_exact_when_solvent` to the
/// Prover over all `u64` (no overflow of the summed liabilities).
#[rule]
pub fn surplus_exact_when_solvent() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let flp_capital: u64 = nondet();
    let insurance: u64 = nondet();

    if let Ok((is_solvent, surplus)) =
        assess_solvency_full(vault, total_collateral, flp_capital, insurance)
    {
        if is_solvent {
            // required = collateral + flp + insurance (no overflow on the solvent branch).
            let required = total_collateral + flp_capital + insurance;
            cvlr_assert!(surplus == vault - required);
            cvlr_assert!(vault >= required);
        }
    }
}

/// Insolvency-detector soundness (the marquee): if the REAL one-sided runtime
/// detector `partial_collateral_proves_insolvent` fires on any real subset of
/// the collateral, the FULL state is genuinely insolvent per the REAL invariant.
/// The detector never false-positives on a solvent protocol — lifted to
/// all-paths over every `u64`.
#[rule]
pub fn insolvency_detector_is_sound() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let partial: u64 = nondet();
    let flp_capital: u64 = nondet();
    let insurance: u64 = nondet();

    // `partial` is a real summed subset of the total collateral.
    cvlr_assume!(partial <= total_collateral);

    if let Ok(true) = partial_collateral_proves_insolvent(partial, flp_capital, insurance, vault) {
        cvlr_assert!(!solvent(vault, total_collateral, flp_capital, insurance));
    }
}
