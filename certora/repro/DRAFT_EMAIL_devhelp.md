# DRAFT — to send from your account (needs your CERTORAKEY/licensed account)

**To:** devhelp@certora.com
**Subject:** Solana Prover — Anchor `error!` global-memcpy blocks pointer analysis (Anchor summary bundle?)

---

Hi Certora team,

We're using the Solana Certora Prover to prove whole-program solvency for an Anchor
program (clober, a Solana CLOB perps DEX). Four solvency rules already pass
non-vacuously on our real invariant symbol, but every rule that executes a real Anchor
handler returns `UNKNOWN` with **"[3308] illegal dereference of an absolute address"** at
a `sol_memcpy_` inside Anchor's `error!`/`#[error_code]` construction.

We've reduced it to a minimal, self-contained reproducer and confirmed the root cause is
the `&'static` global-string copy (message + `file!()` origin) inside
`Error::from(CloberError)` / `CloberError::Display::fmt`, which the pointer analysis
can't classify — and it appears at many inlined sites, so per-function summaries don't
converge.

Shareable report (anonymousKey):
https://prover.certora.com/output/10652951/7b5a39aaf5a24c7089e91fe512a286be?anonymousKey=36da2a88cd4e144bd9959f6a01daf3bd93e4ef40

Environment: certora-cli 8.17.1, cargo-certora-sbf 0.3.5, platform-tools v1.53-certora,
cvlr 0.6.1 / cvlr-solana 0.5.0, anchor-lang 1.x / solana-program 2.x.

We tried (and disproved): function-boundary points-to summaries for
`Error::from`/`Display::fmt`/`ToString`/`Address::*` (the failing global just moves,
never converges); the pointer-analysis/slicer prover_args from your examples
(`-solanaSlicerIter 6`, `-solanaEnablePTAPseudoCanonicalize false`,
`-solanaRemoveCFGDiamonds true`, `-solanaTACOptimize 0`, `-solanaAggressiveGlobalDetection
true`); and stripping `#[msg]` via a cargo feature (no effect — the copy is in Anchor's
core error path, not the message text).

**Question:** is there a canonical Anchor summary bundle (or the specific summary/flag)
that makes the analysis treat the Anchor `error!`/`#[error_code]` construction as
opaque/benign, so handler symbols that contain `require!`/`error!` can be verified? Since
you support Anchor programs, we assume this is a known pattern. A full write-up with the
reproducer files, exact output, and root-cause analysis is attached
(`certora/repro/CERTORA_SUPPORT_REQUEST.md`).

Thanks very much,
<your name>
clober — github.com/Abdr007/clober

---
_Attach: `certora/repro/CERTORA_SUPPORT_REQUEST.md` + optionally point them at
`certora/repro/{minimal_repro.rs,repro.conf}`._
