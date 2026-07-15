# Deployment runbook

This is the operator runbook for a fresh Clober deployment. The generated
[`idl/clober.json`](../idl/clober.json) is the client contract; this repository
does not ship an SDK. Do not substitute example account lists for the IDL when
constructing transactions.

## Preconditions

- A new program keypair and a funded deployer for the target cluster.
- A dedicated upgrade-authority multisig and a separate sequencer key.
- An audited RPC endpoint, transaction sender, alerting, and rollback owner.
- The release artifact, IDL, and the 5--7-market launch catalog approved
  together. The catalog is [`config/mainnet-markets.json`](../config/mainnet-markets.json).

`Anchor.toml` contains localnet and devnet development IDs only. A mainnet
deployment must use the newly generated program key and must not reuse either
development address.

## 1. Verify the release candidate

```bash
cd /path/to/clober
anchor idl build -o /tmp/clober-idl-release.json
node scripts/check-idl-drift.mjs idl/clober.json /tmp/clober-idl-release.json
node scripts/validate-idl-surface.mjs idl/clober.json
node scripts/validate-market-catalog.mjs
for f in $(rg --files -g '*.mjs' er-acceptance sequencer scripts); do node --check "$f"; done
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo audit --deny warnings
cargo build-sbf --tools-version v1.52 \
  --manifest-path programs/clober/Cargo.toml --sbf-out-dir target/deploy
SBF_OUT_DIR=$PWD/target/deploy cargo test -p clober --all-targets
cargo kani --package clober --features no-entrypoint
```

Record the SHA-256 of the exact `target/deploy/clober.so` that passed these
gates. Rebuilds are deployments only after the hash has been compared and
approved by the upgrade-authority multisig.

## 2. Generate and bind a fresh program ID

```bash
solana-keygen new -o keys/clober-mainnet-keypair.json
solana address -k keys/clober-mainnet-keypair.json
```

Update `declare_id!` in `programs/clober/src/lib.rs` and the mainnet deployment
configuration used by the release operator to this address. Regenerate the IDL,
re-run the full release gate above, and confirm the declared ID, binary program
ID, generated IDL metadata, and deployment command agree. This change requires
a full review because the program ID participates in all PDAs.

## 3. Deploy and transfer upgrade authority

Use an isolated signer and the approved binary:

```bash
solana program deploy target/deploy/clober.so \
  --program-id keys/clober-mainnet-keypair.json \
  --url mainnet-beta
solana program show <fresh-program-id> --url mainnet-beta
solana program set-upgrade-authority <fresh-program-id> \
  --new-upgrade-authority <multisig-pda> --url mainnet-beta
```

Verify the deployed program-data authority is the multisig before initializing
assets or accepting deposits. Preserve the deploy transaction, program-data
address, binary hash, IDL hash, and multisig approval in the release record.

## 3.1 Publish and verify the IDL

Publish the exact reviewed IDL only after the program deployment is final.
Clober uses Solana's Program Metadata Program (PMP), not Anchor's deprecated
in-program IDL upload handlers, so the full interface is published without
adding a historical management surface to the trading program:

```bash
npx --yes @solana-program/program-metadata write idl <fresh-program-id> idl/clober.json
```

Record the metadata signature and verify the stored IDL hash against the
reviewed file. Confirm Explorer or other client tooling resolves the program
IDL, instruction names, account names, arguments, events, and typed errors. A
transaction is not launch evidence until its Explorer page shows the expected
program, decoded instruction, account set, logs/events, and successful status.

## 4. Initialize each approved market

For every entry in `config/mainnet-markets.json`, use an IDL-driven operator
client to perform the exact market lifecycle:

1. Create verified token vault accounts for the catalog base mint and USDC
   quote mint.
2. Call `initialize_market` using the catalog mint, bounded `MarketParams`,
   initial price, Pyth receiver configuration, insurance fund, and LP system.
3. Call `init_market_book`, then `init_fill_commitment` and `init_fill_outbox`
   with matching, production-approved capacities. A market is intentionally
   fail-closed until the settlement ring is initialized.
4. Initialize the selected LP accounting system, never both systems for the
   same market. See [OPERATIONS.md](OPERATIONS.md).
5. Configure `init_market_oracle_config`, ingest a fresh Pyth price with
   `update_oracle_from_pyth`, then lock the oracle source once verified.
6. Set the independent sequencer using `set_market_sequencer`, apply the
   authority-transfer process in [OPERATIONS.md](OPERATIONS.md), and check
   `verify_market_invariants`.

The catalog contains real mainnet mints and Pyth feed IDs for BTC, ETH, SOL,
JUP, PYTH, RAY, and BONK. It is an allow-list and parameter review input, not
a transaction generator; confirm each mint and live oracle account on the
target cluster immediately before signing.

## 5. Admission and monitoring

Keep each market closed to public flow until all of these are observed on the
target cluster:

- Pyth updates remain within the configured freshness and confidence bounds.
- The book, fill commitment, and outbox have been initialized and produce
  expected PDAs under the fresh program ID.
- A controlled place, match, settlement, cancel, funding, and withdrawal path
  succeeds; invalid authority and stale-oracle probes fail.
- For every public instruction, execute an approved success or expected-reject
  probe on devnet before mainnet admission. Store its signature, decoded IDL
  instruction, account list, emitted events, compute units, and result in the
  release record. Mainnet uses a controlled subset before public flow; do not
  manufacture transactions solely for Explorer presentation.
- The sequencer, oracle monitor, funding keeper, and alerting each run under
  separately scoped keys.
- Multisig recovery, pause, and authority-transfer drills have been recorded.

Public launch requires an independent audit and an explicit multisig release
approval. Until those external controls exist, this repository is not a
mainnet deployment authorization.
