# Clober

**Verifiable execution for perpetual-futures order books.**

A continuous central-limit order book for perpetual futures. Clober matches on
a MagicBlock Ephemeral Rollup and settles on Solana L1; every fill is
authenticated against an on-chain commitment ring before collateral changes.

| Release posture | Current state |
|---|---|
| Network | Live on Solana devnet; not deployed to mainnet and not externally audited |
| Program | [`8Vdd5n4zbmxqwqY8Xv8JbEcvbih3JsEZzJBtfkoeGp2z`](https://explorer.solana.com/address/8Vdd5n4zbmxqwqY8Xv8JbEcvbih3JsEZzJBtfkoeGp2z?cluster=devnet) |
| Interface | [Generated IDL](idl/clober.json), checked against a fresh Anchor build in CI and published to the canonical devnet Program Metadata account |
| Production gate | Governance multisig migration and the operational checks in [docs/OPERATIONS.md](docs/OPERATIONS.md) |

## Protocol Surface

| | |
|---|---|
| Matching | Price-time priority CLOB on a red-black-tree slab (hypertree) |
| Place at depth | **13.0–14.1k CU, flat across a 511-level book** (O(log n) insertion) |
| Taker sweep | **~14.7k CU base + ~1.2k CU per level crossed**, incl. the per-fill keccak settlement commitment; a 96-level sweep clears in one tx (129k CU) in the default 32 KiB heap |
| Settlement | Two-phase: match on the ER → `apply_fill` on L1 verifies every fill against the keccak commitment ring; a fabricated fill cannot settle |
| Formal verification | Kani proof harnesses on deployed risk and settlement paths, plus 7 Lean proof modules for conservation, funding, credit, realized PnL, and authorization completeness |
| Tests | 621 host/integration tests (the integration suite runs the real compiled `.so` in the BPF VM) + a live MagicBlock devnet ER round-trip acceptance suite |
| Risk engine | Stress-lattice portfolio margin, worse-of(mark, oracle) liquidation pricing, ADL at true bankruptcy, insurance waterfall, junior-claim profit haircut |
| Surface | 162 instructions · 31 accounts · 146 events · 121 typed errors ([IDL](idl/clober.json)) |
| Program | Devnet deployment with canonical on-chain IDL metadata |

Compute methodology: [docs/SETTLEMENT.md](docs/SETTLEMENT.md). Proof
inventory: [docs/FORMAL_VERIFICATION.md](docs/FORMAL_VERIFICATION.md). The
production contract is [INVARIANTS.md](INVARIANTS.md).

## Navigate

| Need | Reference |
|---|---|
| Integrate the program | [IDL](idl/clober.json) and [instruction reference](docs/INSTRUCTIONS.md) |
| Understand settlement | [architecture](docs/ARCHITECTURE.md) and [settlement design](docs/SETTLEMENT.md) |
| Review safety properties | [invariants](INVARIANTS.md), [security policy](SECURITY.md), and [trust boundary](ER_TRUST_BOUNDARY.md) |
| Operate a deployment | [deployment runbook](docs/DEPLOYMENT.md) and [operations guide](docs/OPERATIONS.md) |

## What this is

Clober is the settlement and matching engine — the on-chain program only.
Clients, keepers, bots, and UIs are built against the IDL. The public surface
(IDL, account layouts, events, error codes) is kept stable so any venue can
integrate the book with minimal work.

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
  │ trader states · insurance · LP ·    │    │ fill-commitment ring ·       │
  │ governance · oracle configs          │    │ fill outbox                  │
  │                                      │    │                              │
  │  apply_fill ◀──verify-and-pop─────────────── place_taker_order        │
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
(multisig authority migration):
[docs/OPERATIONS.md](docs/OPERATIONS.md).

## Build & test

```bash
# Build the SBF artifact (platform-tools v1.52 = rustc 1.89; earlier
# releases cannot compile edition2024 dependencies).
cargo build-sbf --tools-version v1.52 \
  --manifest-path programs/clober/Cargo.toml --sbf-out-dir target/deploy

# Host + integration tests (the integration suite loads the compiled .so).
SBF_OUT_DIR=$PWD/target/deploy cargo test -p clober --all-targets

# Formal verification.
cargo kani --package clober --features no-entrypoint

# Live MagicBlock devnet ER acceptance (needs a funded devnet keypair).
cd er-acceptance && npm install && \
  L1_RPC=https://api.devnet.solana.com \
  ER_RPC=https://devnet-as.magicblock.app npm run acceptance
```

Deployment runbook: [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).

## License

MIT, except `programs/clober/src/hypertree/` — vendored from
[Manifest](https://github.com/Bonasa-Tech/manifest) under **GPL-3.0-only**
(see [LICENSE-HYPERTREE](LICENSE-HYPERTREE)).
