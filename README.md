# Flash Book

**The machine-proven on-chain orderbook engine built to power [flash.trade](https://flash.trade).**

A continuous central-limit order book for perpetual futures that matches on a
MagicBlock Ephemeral Rollup and settles on Solana L1, with every fill
authenticated against an on-chain commitment ring. Currently deployed to
devnet; not yet live on flash.trade, not yet externally audited.

## Spec sheet

| | |
|---|---|
| Matching | Price-time priority CLOB on a red-black-tree slab (hypertree) |
| Place at depth | **13.0–14.1k CU, flat across a 511-level book** (O(log n) insertion) |
| Taker sweep | **~14.7k CU base + ~1.2k CU per level crossed**, incl. the per-fill keccak settlement commitment; a 96-level sweep clears in one tx (129k CU) in the default 32 KiB heap |
| Settlement | Two-phase: match on the ER → `apply_fill` on L1 verifies every fill against the keccak commitment ring; a fabricated fill cannot settle |
| Formal verification | **57 Kani proof harnesses** on the deployed risk/settlement paths + **Lean theorems** (haircut conservation, OI/MMR, funding) at the real value domain + Certora property specs |
| Tests | 565 host/integration tests (the integration suite runs the real compiled `.so` in the BPF VM) + a live MagicBlock devnet ER round-trip acceptance suite |
| Risk engine | Stress-lattice portfolio margin, worse-of(mark, oracle) liquidation pricing, ADL at true bankruptcy, insurance waterfall, junior-claim profit haircut |
| Surface | 146 instructions · 137 events · 109 error codes ([IDL](idl/flash_book.json)) |
| Program | `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (devnet) |

Reproduce the CU numbers: [docs/SETTLEMENT.md](docs/SETTLEMENT.md). Proof
inventory: [docs/FORMAL_VERIFICATION.md](docs/FORMAL_VERIFICATION.md).

## What this is

Flash Book is the settlement and matching engine — the on-chain program only.
Clients, keepers, bots, and UIs are built against the IDL. It is being built
to power the Orderbook tab of flash.trade; the public surface (IDL, account
layouts, events, error codes) is kept stable and math-identical to Flash V2
so the venue integrates it with minimal work. See
[docs/V2_INTEGRATION.md](docs/V2_INTEGRATION.md).

- **Hot path on an Ephemeral Rollup.** The order book, fill-commitment ring,
  and fill outbox are delegated to a MagicBlock ER for sub-50ms matching.
  Positions, collateral, and the vault never leave L1.
- **Settlement cannot be forged.** Matching pushes a keccak commitment per
  fill into a FIFO ring; L1 settlement verifies-and-pops each fill against
  it. Ordering and liveness trust the sequencer; fund-safety does not
  ([ER_TRUST_BOUNDARY.md](ER_TRUST_BOUNDARY.md)).
- **Custody never depends on the ER.** Positions and collateral stay on L1, so
  a dark or censoring ER can never take or forge funds. A permissionless
  force-undelegate exit is *designed and Kani-gated* (never fires while the ER
  is live), but it is **not yet executable** against the deployed MagicBlock
  delegation program — undelegation there is validator-driven, so exit from a
  censored/dark ER currently depends on the sequencer signing
  `commit_and_undelegate`. That is a liveness exposure, not a custody one; see
  [ER_TRUST_BOUNDARY.md](ER_TRUST_BOUNDARY.md) §1.1.
- **Risk is proven, not asserted.** Solvency, conservation, margin-floor,
  ring-authenticity, and liveness invariants carry machine-checked proofs
  wired into CI; a regression that breaks an invariant fails the build.
- **Private books.** A market can run on a TEE-backed Private ER where only
  allow-listed readers see depth and flow ([docs/PRIVACY.md](docs/PRIVACY.md)).

## Architecture

```
                    L1 (Solana)                          Ephemeral Rollup
  ┌──────────────────────────────────────┐    ┌──────────────────────────────┐
  │ collateral vault · positions ·       │    │ market book (hypertree) ·    │
  │ trader states · insurance · FLP ·    │    │ fill-commitment ring ·       │
  │ governance · oracle configs          │    │ fill outbox                  │
  │                                      │    │                              │
  │  apply_fill ◀──verify-and-pop─────────────── place_taker_order_v2        │
  │  (ring-authenticated settlement)     │    │ (matching, ~15k CU)          │
  └──────────────────────────────────────┘    └──────────────────────────────┘
```

Full tour: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Instruction
reference: [docs/INSTRUCTIONS.md](docs/INSTRUCTIONS.md). Settlement design
and measured compute: [docs/SETTLEMENT.md](docs/SETTLEMENT.md).

## Security

Status, threat model, accepted trust assumptions, and how to report a
vulnerability: [SECURITY.md](SECURITY.md). The single-sequencer trust
boundary is documented precisely in
[ER_TRUST_BOUNDARY.md](ER_TRUST_BOUNDARY.md) — it is a bounded, stated
assumption, not an omission. Operational steps required before mainnet
(per-market fill-commitment v1 upgrade, multisig authority migration):
[docs/OPERATIONS.md](docs/OPERATIONS.md).

## Build & test

```bash
# Build the SBF artifact (platform-tools v1.52 = rustc 1.89; earlier
# releases cannot compile edition2024 dependencies).
cargo build-sbf --tools-version v1.52 \
  --manifest-path programs/flash-book/Cargo.toml --sbf-out-dir target/deploy

# Host + integration tests (the integration suite loads the compiled .so).
SBF_OUT_DIR=$PWD/target/deploy cargo test -p flash-book --all-targets

# Formal verification.
cargo kani --package flash-book --features no-entrypoint

# Live MagicBlock devnet ER acceptance (needs a funded devnet keypair).
cd er-acceptance && npm install && \
  L1_RPC=https://api.devnet.solana.com \
  ER_RPC=https://devnet-as.magicblock.app npm run acceptance
```

Deployment runbook: [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).

## License

MIT, except `programs/flash-book/src/hypertree/` — vendored from
[Manifest](https://github.com/Bonasa-Tech/manifest) under **GPL-3.0-only**
(see [LICENSE-HYPERTREE](LICENSE-HYPERTREE)).
