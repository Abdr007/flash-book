# Security Policy

## Status

This project is on devnet only. It has not been independently audited.
The README and `docs/PRODUCTION_READINESS.md` are explicit about what
the protocol does and does not yet guarantee. Do not deploy to mainnet
or hold real value against the current code.

## Reporting a vulnerability

If you find a security issue:

1. **Do not open a public GitHub issue.**
2. Open a private security advisory on GitHub:
   https://github.com/Abdr007/flash-book/security/advisories/new
3. Or email the maintainers (address in `Cargo.toml` / `package.json`
   author field).

Include:

- A clear description of the vulnerability and impact.
- A minimal reproduction (test case or PoC tx).
- Whether the issue is exploitable on devnet today, on a hypothetical
  mainnet deployment of the current code, or only theoretical.

We aim to acknowledge within 72 hours.

## Scope

### In scope

- The Anchor program in `programs/flash-book/`. Anything that lets a
  user cause incorrect collateral movement, position state, oracle
  acceptance, liquidation reward routing, or account-control bypass.
- The risk math in `programs/flash-book/src/matcher/risk.rs`. Anything
  that violates the invariants documented in `docs/MARGIN_MATH.md §9`.
- The SDK in `sdk-ts/` if it produces transactions that bypass intended
  on-chain checks (e.g. wrong account layout, missing constraint).
- The keeper code in `bot/` if it can be coerced into actions that
  harm users (e.g. firing liquidations on healthy positions).

### Out of scope

- Issues that require a malicious validator. Solana validator security
  is upstream.
- DoS attacks via the public RPC. RPC-layer mitigation is the
  operator's responsibility.
- Anything in the TypeScript reference simulator (`src/`) — it's a
  research artifact, not the production code path.

## Known limitations (not vulnerabilities — documented gaps)

These are tracked in code comments and `docs/`. Reporting them is
welcome but they are not new findings:

- **Sub-account fill routing trusts the off-chain sequencer.** Phase 2d
  relaxed `taker_trader_state` / `maker_trader_state` seeds; the
  handler verifies `trader_state.trader == order.trader` but not
  `trader_state.key() == find_pda([STATE_SEED, trader, &[sub_index]])`.
  Documented in `docs/SUB_ACCOUNT_TRADING.md` and the COMPARISON.md
  weaknesses section.
- **No on-chain FBA or commit-reveal.** Both are in the TypeScript
  simulator only.
- **No mainnet deployment / no audit.** Tracked in
  `docs/PRODUCTION_READINESS.md`.

## Disclosure policy

We follow a coordinated-disclosure approach. After a fix is shipped to
devnet:

- 14-day window for reporters to coordinate before public disclosure.
- Patch commits and advisories will credit the reporter unless they
  prefer anonymity.

## Supported branches

`main` is the only branch receiving security fixes. Forks and
historical tags are not supported.
