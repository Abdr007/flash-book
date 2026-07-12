# Launch framing — the honest posture

This is the one-page truth about what Clober is today. It is also the
operating norm: **no public word ships that we cannot prove on demand.**

## What Clober is

A fully on-chain central-limit-order-book perpetuals DEX on Solana, with a
hypertree order book, a stress-lattice risk engine, and sub-50ms fills via a
MagicBlock Ephemeral Rollup. **Its accounting is machine-proven.**

## The one claim we build everything toward

> Every money-moving instruction is machine-proven to preserve solvency, on a
> fully on-chain order book with sub-50ms fills, and the Hyperliquid-$20M oracle
> manipulation is proven impossible.

Each clause links to a committed, CI-gated proof — Kani (bounded model checking)
and Lean 4 + Mathlib (unbounded, real divisors) — see `docs/FORMAL_VERIFICATION.md`
and `formal_verification/`. The proofs run on every PR; a `sorry` or a broken
proof fails the build.

## What is true today

- **Deployed on devnet**, program `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq`.
- **The core accounting is proven**: solvency conservation, no self-liquidation
  onto insurance, manipulated-market credit collapse, margin frame-stability,
  funding zero-sum, realized-PnL value, haircut junior-claim bounds, insurance/LP
  isolation — all machine-checked, all CI-gated.
- **Reproducible**: the IDL is regenerated and drift-gated in CI; the event-replay
  reconciler rebuilds all 8 state dimensions byte-for-byte from events.

## What is NOT true yet — stated plainly

- **Not audited.** The turnkey audit package is ready; the firm's signature is an
  honest external wait. Until it closes, "audited" is not claimable. (6.1)
- **Not on mainnet.** Mainnet follows the audit close, not a date.
- **Two honest vendor dependencies remain**, both code-complete on our side:
  the audit signature (6.1), and MagicBlock owner-recovery for the trustless
  force-undelegate censorship-exit (2.5). We do not overstate either.
- **Some features are declared post-launch roadmap**, not silently missing:
  on-chain copy-vaults, permissionless (HIP-3-style) market deploy, and
  decentralized-sequencer activation. Declaring them beats rushing them.

## The invitation

**Run it, read it, break it.** The source is open, the proofs are reproducible,
the IDL is the contract, and the failure modes are documented (`docs/GOTCHAS.md`).
An agent or a human who wants a venue whose solvency is a *theorem* rather than a
*promise* should test that claim adversarially — that is precisely the point.

## The trust wedge

Settlement and delisting always resolve at a robust oracle, with no discretionary
repricing — ever (`docs/SETTLEMENT_POLICY.md`). We cannot do a validator-put on a
market. That is a permanent, structural commitment, not a policy that can be
revised under pressure.
