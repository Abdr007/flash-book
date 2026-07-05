# Security Policy

## Status

Devnet only. Not independently audited. Do not deploy to mainnet or hold
real value against the current code.

The engine is code-complete for its current scope, with machine-checked
invariants in CI (see `docs/FORMAL_VERIFICATION.md`). The gates that remain
before real value are a professional external audit and the operational
steps in `docs/OPERATIONS.md` (per-market fill-commitment v1 upgrade,
multisig authority migration).

## Reporting a vulnerability

1. **Do not open a public GitHub issue.**
2. Open a private security advisory:
   https://github.com/Abdr007/flash-book/security/advisories/new
3. Or email the maintainers (address in the `Cargo.toml` author field).

Include a clear description + impact, a minimal reproduction (test or PoC
tx), and whether it is exploitable on devnet today, on a hypothetical
mainnet deployment of the current code, or only theoretical. We aim to
acknowledge within 72 hours.

## Scope

### In scope

- The Anchor program in `programs/flash-book/`: anything that lets a user
  cause incorrect collateral movement, position state, oracle acceptance,
  liquidation-reward routing, or account-control bypass.
- The risk math in `programs/flash-book/src/matcher/`: anything that
  violates the invariants in `docs/MARGIN_MATH.md` / `docs/HAIRCUT_MATH.md` /
  `docs/SAFETY.md`. The conservation and solvency invariants are
  machine-checked — a violation that Kani/Lean should have caught is a
  doubly interesting report.
- The ER boundary: anything that lets a sequencer or any third party forge,
  alter, replay, or reroute a settled fill, corrupt committed state that the
  program then accepts, or trap funds past the censorship escape
  (`ER_TRUST_BOUNDARY.md`).

### Out of scope

- Issues requiring a malicious Solana validator (upstream security).
- DoS via public RPC (operator responsibility).
- Off-chain tooling (clients, bots, keepers) — not in this repository.
- Sequencer reordering/censorship *within* the documented trust model
  (see below) — the oracle-band mark clamp mitigates a manipulated mark, but
  ordering itself is trusted, and the permissionless censorship *exit* is not
  yet executable against the deployed delegation program (see
  `ER_TRUST_BOUNDARY.md` §1.1: exit currently depends on sequencer
  cooperation — a liveness exposure). A report demonstrating trapped funds
  under a dark/censoring ER is in scope as a known-liveness item, not a new
  finding.

## Accepted trust assumptions (documented, not findings)

These are deliberate, bounded design decisions. They are presented here so
the code and the documentation tell the same story an external auditor will:

- **Single-sequencer ordering/liveness.** Fill ordering and matching
  liveness trust the market's configured sequencer. Fund-safety does not:
  settlement authenticity is enforced on L1 by the fill-commitment ring
  (`apply_fill` verifies every fill), the sequencer cannot route a fill to
  the wrong account (trader-state PDAs are re-derived at settlement), and a
  censoring or dead ER can be permissionlessly force-undelegated after a
  proven-silent timeout. Full statement: `ER_TRUST_BOUNDARY.md` §1.
- **TEE privacy enforcement is validator-side.** For private books, the
  on-chain program manages the permission allow-list; the read-gating
  itself is enforced by the MagicBlock Private ER (TEE). `docs/PRIVACY.md`.
- **Oracle trust.** Markets consume Pyth pull or Pyth Lazer prices under
  staleness, confidence, and per-slot move gates, or an authority-pushed
  price on markets that have not locked the oracle source; `lock_oracle_source`
  removes the authority path one-way per market.

## External audit

Not yet engaged. The audit entry points are: `docs/SAFETY.md` (threat model
and invariants), `ER_TRUST_BOUNDARY.md` (trust boundary + what is proven
where), `docs/SETTLEMENT.md` (settlement authenticity design),
`docs/FORMAL_VERIFICATION.md` (proof inventory), and the reproducible test
and CU commands in the README.

## Disclosure policy

Coordinated disclosure: a 14-day window for reporters before public
disclosure after a devnet fix ships; reporters credited unless they prefer
anonymity.

## Supported branches

`main` only. Forks and historical tags are not supported.
