//! Certora Solana Prover harness for P-SOLV-4 (global solvency preservation).
//!
//! STATUS: scaffold — NOT compiled by the normal cargo build (no `cvlr`
//! dependency is wired, and this file is not in any module tree). It becomes
//! live once a Certora Solana license + the `cvlr` SDK are added; see README.md
//! in this directory. It is kept out of `programs/flash-book/src` deliberately
//! so the production build and CI stay green without the proprietary SDK.
//!
//! The harness reuses the ALREADY-KANI-PROVEN pure cores so the Prover only has
//! to discharge the *reachability / all-paths* obligation, not re-derive the
//! arithmetic:
//!   - `matcher::insurance::assess_solvency_full`              (full invariant)
//!   - `matcher::insurance::partial_collateral_proves_insolvent` (one-sided)
//!
//! Proof obligation set — the 19 balance-mutating instructions (every handler
//! that can move value across the vault boundary). The other ~100 handlers are
//! view/admin/order-book ops and are closed trivially by `solvencyPreserved`.

#![cfg(feature = "certora")]

use cvlr::prelude::*;
use flash_book::matcher::insurance::{assess_solvency_full, partial_collateral_proves_insolvent};

/// Ghost reads over the harness's symbolic account set. In the real run these
/// are `solana_summaries` that pull the live TokenAccount balance, the
/// InsuranceFund / FlpExposure fields, and the summed TraderState + isolated
/// Position collateral. Here they are nondet stand-ins constrained by the rule.
struct Ledger {
    vault: u64,
    total_collateral: u64,
    flp_capital: u64,
    insurance: u64,
}

impl Ledger {
    fn solvent(&self) -> bool {
        matches!(
            assess_solvency_full(self.vault, self.total_collateral, self.flp_capital, self.insurance),
            Ok((true, _))
        )
    }
}

/// Inductive preservation: if the ledger is solvent before an instruction, it is
/// solvent after. `cvlr` dispatches `f` over every instruction entrypoint; the
/// Prover must show no path falsifies the post-condition.
#[rule]
pub fn solvency_preserved(f: Instruction) {
    let pre = nondet_ledger();
    cvlr_assume!(pre.solvent());

    let post = execute_instruction(f, pre);

    cvlr_assert!(post.solvent());
}

/// The runtime one-sided sweep is sound: whenever `verify_collateral_solvency`
/// would error (detector fires), the full invariant is genuinely violated for
/// the real total. Lifts the Kani `partial_insolvency_detector_is_sound` to the
/// instruction boundary (the on-chain `require!(!insolvent, ProtocolInsolvent)`).
#[rule]
pub fn runtime_detector_matches_invariant() {
    let l = nondet_ledger();
    let partial: u64 = nondet();
    cvlr_assume!(partial <= l.total_collateral); // a real summed subset

    if let Ok(true) = partial_collateral_proves_insolvent(partial, l.flp_capital, l.insurance, l.vault) {
        cvlr_assert!(!l.solvent());
    }
}

// ── Harness plumbing (provided by the license-time wiring) ──────────────────
// `nondet_ledger`, `execute_instruction`, the `Instruction` enum, and the
// account-summary bindings are generated from the program IDL + build script.
// They are declared here as the contract the README's setup must satisfy.
extern "C" {
    fn nondet_ledger() -> Ledger;
    fn execute_instruction(f: Instruction, pre: Ledger) -> Ledger;
}
type Instruction = u8; // placeholder: the IDL-derived discriminant set
