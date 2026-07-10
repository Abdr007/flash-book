//! MINIMAL Certora Solana Prover reproducer for the Anchor `error!` global-memcpy
//! pointer-analysis failure. Compiled into the flash-book-certora-harness crate.
//! Add `repro_anchor_error_memcpy` to a conf's `rule` set and run certoraSolanaProver.
//!
//! Expected: UNKNOWN — "[3308] illegal dereference of an absolute address ... at
//! call sol_memcpy_" — even though the assertion is trivially true. The ONLY cause
//! is the pointer analysis of the Anchor `require!`/`error!(FlashBookError::…)`
//! construction (which memcpy's a &'static global), reached via check_simple_withdraw.
use cvlr::prelude::*;
use flash_book::xmargin::check_simple_withdraw;

#[rule]
pub fn repro_anchor_error_memcpy() {
    let collateral: u64 = nondet();
    let amount: u64 = nondet();
    let er_reserved: u64 = nondet();
    // Merely EVALUATING this real gate (which contains `require!`/`error!`) makes the
    // Prover analyze the Anchor error-construction path → the global-memcpy deref.
    let _ = check_simple_withdraw(collateral, amount, er_reserved).is_ok();
    cvlr_assert!(true); // trivial: the rule fails at pointer-analysis, not this assert
}
