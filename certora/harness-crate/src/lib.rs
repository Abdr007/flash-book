//! Certora Solana Prover harness for Flash Book P-SOLV-4 (global solvency).
//!
//! Separate verification-only crate (see Cargo.toml for why it is not a module
//! of flash-book). Each `#[rule]` establishes a solvent pre-state via the REAL,
//! Kani-proven `assess_solvency_full`, executes a balance-mutating effect with
//! fully symbolic (`nondet`) inputs, then asserts the real invariant still
//! holds — the all-paths preservation obligation Kani cannot give.
//!
//! COVERAGE (honest): the withdraw slice (1 of the 19 balance-mutating
//! instructions in ../harness/README.md). NOT the full parametric-over-19
//! proof — that remains staged.

use cvlr::prelude::*;
use flash_book::matcher::insurance::assess_solvency_full;

/// Solvent per the REAL invariant function `assess_solvency_full`
/// (flash_book::matcher::insurance) — NOT an inline re-derivation. This is the
/// exact P-SOLV-4 predicate the on-chain `verify_collateral_solvency` enforces.
fn solvent(vault: u64, total_collateral: u64, flp_capital: u64, insurance: u64) -> bool {
    matches!(
        assess_solvency_full(vault, total_collateral, flp_capital, insurance),
        Ok((true, _))
    )
}

/// P-SOLV-4, withdraw path: if the protocol is solvent per the REAL invariant
/// `assess_solvency_full`, then a collateral withdrawal (the same `amount`
/// leaves the quote vault AND the trader's collateral ledger) leaves the
/// protocol solvent per the REAL invariant — for ALL `u64` states.
///
/// The withdraw precondition `amount <= total_collateral` is the numeric core
/// of the on-chain gate `xmargin::check_simple_withdraw` (`amount <=
/// collateral`, aggregated). Calling that gate symbol directly is blocked today
/// by a Prover pointer-analysis limit (its Anchor `require!` error path
/// memcpy's an unclassified `solana-address` global) — tracked as the next step
/// toward per-handler coverage.
#[rule]
pub fn solvency_preserved_simple_withdraw() {
    // Fully symbolic pre-state over the whole u64 domain.
    let vault: u64 = nondet();
    let total_collateral: u64 = nondet();
    let flp_capital: u64 = nondet();
    let insurance: u64 = nondet();
    let amount: u64 = nondet();

    // Solvent before, per the REAL invariant.
    cvlr_assume!(solvent(vault, total_collateral, flp_capital, insurance));
    // Withdraw precondition (numeric core of the real `check_simple_withdraw`
    // gate): a trader can only release collateral it holds.
    cvlr_assume!(amount <= total_collateral);

    // The withdrawal's real effect: the same `amount` leaves the vault and the
    // trader's collateral ledger. (Solvency ⇒ vault ≥ total_collateral ≥
    // amount, so neither subtraction underflows.)
    let vault_post = vault - amount;
    let total_collateral_post = total_collateral - amount;

    // Solvent after, per the REAL invariant.
    cvlr_assert!(solvent(
        vault_post,
        total_collateral_post,
        flp_capital,
        insurance
    ));
}
