# Security Policy

## Status

Devnet only. Not independently audited. Do not deploy to mainnet or hold
real value against the current code.

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
  violates the invariants in `docs/MARGIN_MATH.md` / `docs/HAIRCUT_MATH.md`.
  The haircut conservation + solvency invariants are machine-checked —
  see `docs/FORMAL_VERIFICATION.md`.

### Out of scope

- Issues requiring a malicious validator (upstream Solana security).
- DoS via public RPC (operator responsibility).
- Off-chain tooling (clients, bots, keepers) — not in this repository.

## Known limitations (documented gaps, not findings)

- **No mainnet deployment / no external audit.** Internal audit:
  `docs/AUDIT.md`.
- **The off-chain sequencer is a single point of trust** for fill
  ordering. The program authenticates it (`MarketAccount.sequencer`) and
  re-derives the trader-state PDA from `(trader, sub_index)` in
  `apply_fill` / `apply_flp_fill` (`verify_trader_state_pda`), so a
  hostile sequencer cannot route a fill to the wrong account — but it can
  still reorder or censor. Decentralizing the sequencer is future work.
- **No FBA / commit-reveal on-chain — by design.**

## Disclosure policy

Coordinated disclosure: a 14-day window for reporters before public
disclosure after a devnet fix ships; reporters credited unless they
prefer anonymity.

## Supported branches

`main` only. Forks and historical tags are not supported.
