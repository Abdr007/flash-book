# Flash Book — Audit Outreach (next step: book the audit)

The audit queue is the long pole on the entire timeline (typically 2–6 weeks just
to get a slot). Book it now; everything else (bug bounty, launch prep) sequences
behind it. The scope package the firm needs is `docs/AUDIT_SCOPE.md`.

## Firm shortlist (Solana-specialized)

| Firm | Notes | Typical lead time |
|---|---|---|
| **OtterSec** | Deep Solana/Anchor + perps DEX experience; strong on settlement/risk math | 2–5 wks |
| **Zellic** | Strong on novel mechanisms + formal-methods-friendly (our FV suite helps) | 2–5 wks |
| **Neodyme** | Solana-native, known for CU/runtime + account-model edge cases | 2–6 wks |
| **Trail of Bits** | Heavyweight; pairs well with our Kani/Lean work; longer queue/cost | 4–8 wks |
| **Sec3 / others** | Cheaper/faster second opinions; consider for a parallel pass | varies |

Recommendation: get quotes from **2** (e.g. OtterSec + Zellic) in parallel —
booking two slots hedges the queue and a second perspective is cheap insurance for
a fund-custody program.

## What to send (all ready in-repo)
- `docs/AUDIT_SCOPE.md` — scope, focus areas, FV evidence, the **explicit
  not-proven list** (prime targets), residuals, deployment plan.
- Repo read access to branch `fix-security-c1-c2` (PR #34) + the squash-diff vs `main`.
- `docs/FLASHBOOK_SECURITY.md` (threat model + remediation ledger),
  `certora/PROPERTIES.md` (invariant status).
- Reproduce commands (build + 432 tests + 31 Kani + 7 Lean + CU benchmark).

## Outreach email template (fill the [brackets], then send)

> **Subject:** Audit request — Flash Book, on-chain CLOB perps DEX (Solana/Anchor)
>
> Hi [firm] team,
>
> We're seeking a security audit of **Flash Book**, a fully on-chain central-limit-
> order-book perpetual-futures DEX on Solana (Anchor 0.31.1, ~32k LOC (32459 lines)). The code is
> on a single review branch and is **audit-ready**: full test suite green, plus a
> machine-checked formal-verification suite (31 Kani proofs + 7 Lean theorems,
> CI-gated).
>
> **Scope:** the deployed Anchor program (`programs/flash-book`). One self-contained
> scope document (`AUDIT_SCOPE.md`) frames the review surface, the funds/settlement
> focus areas, the FV evidence, and — importantly — an explicit list of invariants
> we have *not* proven (runtime-enforced only) that we'd want the most scrutiny on.
>
> **Recent work to review:** an adversarial hardening pass (2 critical + 10 high,
> all remediated), a settlement-authenticity redesign that removes sequencer
> fill-fabrication, and a book-stuffing mitigation.
>
> **Context:** the program is **currently deployed** (devnet → mainnet upgrade
> planned post-audit), so this is a pre-upgrade audit gating a guarded launch.
>
> Could you share **availability (earliest start), turnaround, and a quote** for a
> program of this size? Happy to grant repo access and walk the team through the
> scope doc. Targeting a start in [timeframe].
>
> Thanks,
> [name / handle], Flash Book

## Decisions only you can make
- **Which firm(s)** + budget (audits for a program this size are typically
  $30k–$120k+ depending on firm/scope/turnaround).
- **Timeline target** for the start.
- Whether to do a **public bug bounty** (Immunefi) in parallel with / after the
  audited build (Phase 2).

I can't send this or sign anything — but the moment you pick a firm and target
date, fill the brackets and it's ready to go.
