//! Certora Solana Prover harness for Clober P-SOLV-4 (global solvency).
//!
//! Separate verification-only crate (see Cargo.toml for why it is not a module
//! of clober). Every rule establishes its pre-state via the REAL,
//! Kani-proven solvency cores in `clober::matcher::insurance`, executes a
//! balance-mutating effect with fully symbolic (`nondet`) inputs, and asserts
//! the real invariant still holds — the all-paths obligation Kani cannot give.
//!
//! P-SOLV-4 invariant:  vault ≥ Σ collateral + Σ lp_capital + insurance,
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
use clober::matcher::insurance::{assess_solvency_full, partial_collateral_proves_insolvent};
use clober::xmargin::check_simple_withdraw;

/// Solvent per the REAL invariant function `assess_solvency_full` — NOT an
/// inline re-derivation. This is the exact P-SOLV-4 predicate the on-chain
/// `verify_collateral_solvency` sweep enforces.
fn solvent(vault: u64, total_collateral: u64, lp_capital: u64, insurance: u64) -> bool {
    matches!(
        assess_solvency_full(vault, total_collateral, lp_capital, insurance),
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
    let lp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let amount: u64 = nondet();

    cvlr_assume!(solvent(vault, total_collateral, lp_capital, insurance));
    cvlr_assume!(amount <= total_collateral);

    // Solvency ⇒ vault ≥ total_collateral ≥ amount, so neither subtraction underflows.
    let vault_post = vault - amount;
    let total_collateral_post = total_collateral - amount;

    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        lp_capital,
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
    let lp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let amount: u64 = nondet();

    cvlr_assume!(solvent(vault, total_collateral, lp_capital, insurance));
    // Real deposits cannot overflow the u64 vault / ledger (checked on-chain).
    cvlr_assume!(vault.checked_add(amount).is_some());
    cvlr_assume!(total_collateral.checked_add(amount).is_some());

    let vault_post = vault + amount;
    let total_collateral_post = total_collateral + amount;

    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        lp_capital,
        insurance
    ));
}

/// Surplus exactness: when solvent, the surplus the REAL `assess_solvency_full`
/// returns is EXACTLY `vault − (collateral + lp + insurance)` — no value is
/// invented or destroyed. Lifts the Kani `surplus_exact_when_solvent` to the
/// Prover over all `u64` (no overflow of the summed liabilities).
#[rule]
pub fn surplus_exact_when_solvent() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let lp_capital: u64 = nondet();
    let insurance: u64 = nondet();

    if let Ok((is_solvent, surplus)) =
        assess_solvency_full(vault, total_collateral, lp_capital, insurance)
    {
        if is_solvent {
            // required = collateral + lp + insurance (no overflow on the solvent branch).
            let required = total_collateral + lp_capital + insurance;
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
    let lp_capital: u64 = nondet();
    let insurance: u64 = nondet();

    // `partial` is a real summed subset of the total collateral.
    cvlr_assume!(partial <= total_collateral);

    if let Ok(true) = partial_collateral_proves_insolvent(partial, lp_capital, insurance, vault) {
        cvlr_assert!(!solvent(vault, total_collateral, lp_capital, insurance));
    }
}

/// P-SOLV-4, withdraw path calling the REAL on-chain Anchor gate symbol
/// `xmargin::check_simple_withdraw` DIRECTLY (not modeling its precondition).
/// This is what a full per-Anchor-handler proof requires.
///
/// STATUS: **BLOCKED — not in the conf's rule set (never run in CI).** The
/// Prover returns UNKNOWN with "illegal dereference of an absolute address":
/// Anchor's `#[error_code]` construction copies the `#[msg("...")]` `&'static`
/// global strings (`error!` → `Error::from(CloberError)` → `to_string()` →
/// `Display::fmt` → `write_str`), scattered across many inlined sites, which the
/// pointer analysis cannot classify. Summarizing individual boundaries only
/// moves the failing global (0x532a → 0x51f0 → 0x5880 …); it does not converge,
/// and the documented pointer-analysis / slicer `prover_args` do not resolve it.
/// Closing this needs Certora's Anchor summary bundle (devhelp@certora.com) or
/// stripping the `#[msg]` strings under the certora feature. Left here as the
/// exact, reproducible G1-closing residual. The four rules above already prove
/// the solvency PROPERTY these handlers must preserve, on the real invariant.
#[allow(dead_code)]
#[rule]
pub fn solvency_preserved_withdraw_gate() {
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let lp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let trader_collateral: u64 = nondet();
    let er_reserved: u64 = nondet();
    let amount: u64 = nondet();

    // Valid pre-state: this trader's collateral is part of the aggregate.
    cvlr_assume!(trader_collateral <= total_collateral);
    // Solvent before, per the REAL invariant.
    cvlr_assume!(solvent(vault, total_collateral, lp_capital, insurance));
    // The REAL on-chain withdraw gate permits this withdrawal.
    cvlr_assume!(check_simple_withdraw(trader_collateral, amount, er_reserved).is_ok());

    // Gate ⇒ amount ≤ trader_collateral ≤ total_collateral, and solvency ⇒
    // vault ≥ total_collateral ≥ amount, so neither subtraction underflows.
    let vault_post = vault - amount;
    let total_collateral_post = total_collateral - amount;

    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        lp_capital,
        insurance
    ));
}

/// MINIMAL reproducer for the Certora Anchor `error!` global-memcpy blocker.
/// See certora/repro/. Evaluating the real `check_simple_withdraw` (which uses
/// `require!`/`error!`) makes the Prover analyze Anchor error construction and
/// return UNKNOWN "illegal dereference of an absolute address", despite the
/// trivial assertion. Isolates the SOLE cause for the Certora support request.
#[allow(dead_code)]
#[rule]
pub fn repro_anchor_error_memcpy() {
    let collateral: u64 = nondet();
    let amount: u64 = nondet();
    let er_reserved: u64 = nondet();
    let _ = check_simple_withdraw(collateral, amount, er_reserved).is_ok();
    cvlr_assert!(true);
}
