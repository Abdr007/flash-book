# Certora Solana Prover — Anchor `error!` global-memcpy pointer-analysis failure

**Ask:** an Anchor-aware summary bundle (or the correct summary/flag) that lets the
pointer analysis get past the `&'static` global-string copy inside Anchor's
`error!`/`#[error_code]` construction, so rules that call real Anchor handler
symbols verify instead of returning `UNKNOWN`.

This blocks the whole-program solvency proof for **flash-book** (Solana CLOB perps).
Four solvency rules already pass on the real invariant symbol; the remaining ~47
money-path sites are inline mutations inside Anchor `Context` handlers, and any rule
that executes one hits the failure below.

---

## Environment (all current)
| Component | Version |
|---|---|
| `certora-cli` | 8.17.1 |
| `cargo-certora-sbf` | 0.3.5 |
| platform-tools | `v1.53-certora` (osx-aarch64; v1.52 is not published for this arch) |
| `cvlr` | `cvlr-v0.6.1` (git) |
| `cvlr-solana` | `cvlr-solana-v0.5.0` (git) |
| Program | Anchor `anchor-lang 1.x`, solana-program 2.x |

## Minimal reproducer (self-contained)
Rule (`certora/repro/minimal_repro.rs`, compiled into the `flash-book-certora-harness` crate):
```rust
use cvlr::prelude::*;
use flash_book::xmargin::check_simple_withdraw;

#[rule]
pub fn repro_anchor_error_memcpy() {
    let collateral: u64 = nondet();
    let amount: u64 = nondet();
    let er_reserved: u64 = nondet();
    // Merely EVALUATING this real gate (it contains `require!`/`error!`) makes the
    // Prover analyze Anchor's error construction → the global-memcpy deref.
    let _ = check_simple_withdraw(collateral, amount, er_reserved).is_ok();
    cvlr_assert!(true);            // trivial — the rule fails at pointer-analysis, not here
}
```
`check_simple_withdraw` is a trivial real function:
```rust
pub fn check_simple_withdraw(collateral: u64, amount: u64, er_reserved: u64) -> Result<()> {
    require!(amount <= collateral, FlashBookError::InsufficientCollateral); // -> error!(...)
    let remaining = collateral - amount;
    require!(remaining >= er_reserved, FlashBookError::ErMarginReserved);
    Ok(())
}
```
Conf: `certora/repro/repro.conf` (rule + `-solanaAggressiveGlobalDetection true`).
Run: `certoraSolanaProver certora/repro/repro.conf --wait_for_results` from the crate dir.

## Observed result (reproduced repeatedly)
```
[rule] repro_anchor_error_memcpy: UNKNOWN
[3308] illegal dereference of an absolute address
  source: .../solana-address-2.6.1/src/syscalls.rs  (also surfaces at the program's
          error-construction sites, e.g. matcher/funding.rs:93 `.or_overflow()?`)
  from:   core/src/ptr/const_ptr.rs:1304
  from:   alloc/src/slice.rs:454
note: A dereference of an absolute address whose exact memory segment (heap, globals,
      etc.) is not statically known.
Dev message: Pointer domain: dereference of an absolute address 22976 (0x59c0) at call
      sol_memcpy_ /*unhoisted_memcpy*/
Warning: The following functions are neither inlined nor summarized. They are treated as
      external. [<anchor_lang_error::Error as From<anchor_lang_error::AnchorError>>::from,
      <flash_book::errors::FlashBookError as core::fmt::Display>::fmt]
```
Shareable report (anonymousKey — openable without our account):
`https://prover.certora.com/output/10652951/7b5a39aaf5a24c7089e91fe512a286be?anonymousKey=36da2a88cd4e144bd9959f6a01daf3bd93e4ef40`

## Root cause (our analysis)
Anchor's `error!(FlashBookError::…)` → `Error::from(FlashBookError)` builds an
`AnchorError` whose `error_msg`/origin come from `#[msg("…")]` and `file!()`
**`&'static` globals**, copied via `to_string()`→`Display::fmt`→`write_str` and stored,
at **many `#[inline]`d sites** across the program. The pointer analysis cannot classify
the source global at the `sol_memcpy_`, so any path that constructs a `FlashBookError`
(i.e. every `require!`/`.or_overflow()?`) yields `UNKNOWN`.

## Workarounds we tried and DISPROVED (so the ask is precise)
1. **Function-boundary points-to summaries** — `<Error as From<AnchorError>>::from`,
   `<FlashBookError as Display>::fmt`, `<FlashBookError as ToString>::to_string`,
   `solana_address::Address::{find,try_find,create}_program_address`. Each provably
   works (the failing global address *moves*: `0x532a`→`0x51f0`→`0x5880`→`0x59c0`), but
   it never converges — the copies are scattered across inlined sites.
2. **Prover args from your examples** — `-solanaSlicerIter 6`,
   `-solanaEnablePTAPseudoCanonicalize false`, `-solanaRemoveCFGDiamonds true`,
   `-solanaTACOptimize 0`, `-solanaAggressiveGlobalDetection true`. No effect.
3. **Stripping `#[msg]` under a cargo feature** (two `#[cfg]`-gated enum copies). No
   effect — disproved that the message string is the culprit; the copy is in Anchor's
   *core* error path (origin/name/`AnchorError` internals), not the `#[msg]` text.

## The specific request
The canonical Anchor summary set (or the single flag/summary) that makes the pointer
analysis treat the Anchor `error!`/`#[error_code]` construction as opaque/benign, so
`check_simple_withdraw` (and the ~47 handler sites like it) can be verified. You verify
Anchor programs, so this is presumably a solved pattern on your side.
