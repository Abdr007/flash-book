# Live-ER acceptance harness — Pinocchio port (`flash-book-pin`)

Validates the one thing `solana-program-test` structurally **cannot**: the real
MagicBlock CPI **delegation round-trip**. This is the only part of the pin ER work
(PR #190) not covered by the unit/integration suite — it requires a live MagicBlock
devnet ER.

Unlike the Anchor `../er-acceptance` suite (which uses the IDL + Anchor method
builders), the pin program is a **raw 1-byte-Ix-tag program with no IDL**, so this
harness builds raw instructions (tag byte + LE data + account metas) directly.

## What it asserts (the PR #190 round-trip)

On a market whose book already exists on L1:

1. **L1 precheck** — the book PDA exists and is program-owned (not already delegated).
2. **L1 → DLP** — `delegate_market_book`: the WAVE-24i `cpi_delegate` staging
   (create the owner-program buffer PDA → copy the book in → zero it → assign to the
   delegation program under the PDA seeds → CPI Delegate → close the buffer). The bug
   #190 fixed: the old port did a bare CPI with a 1-byte disc and no staging, which
   the upgraded DLP rejects.
3. **ER** — `place_limit_order` rests a bid **on the rollup**, mutating the delegated
   book (proves the ER owns and can write it). A resting limit touches only the book,
   not `market` OI, so no writable-mix delegation of the market is needed.
4. **ER → L1** — `commit_market_book` snapshots the book; assert it is still
   DLP-owned (delegated).
5. **ER → L1** — `commit_and_undelegate_market_book` → the DLP's
   `process_undelegation` callback re-opens the book program-owned and runs the #188
   `validate_node_links` defense on the committed state; assert the book is back under
   the program and non-empty (the undelegate finalized cleanly).

> Note: the pin **L1-initiated** `undelegate_market_book` / `force_undelegate_market_book`
> intentionally fail closed with `Custom(221)` (Anchor removed them) — undelegation flows
> through `commit_and_undelegate_*` → `process_undelegation`, which is what this harness
> exercises.

## Prerequisites (one-time, on L1)

This harness drives the round-trip on an **existing** market+book. Set those up first:

1. Deploy the pin program: `cargo build-sbf --manifest-path programs/flash-book-pin/Cargo.toml`
   then `solana program deploy …/flash_book_pin.so` (note the program id).
2. `initialize_insurance_fund` (the CR-1 gate requires `insurance.authority == authority`).
3. `initialize_market` (Ix 11) — your wallet becomes the market authority + sequencer.
4. `init_market_book` (Ix 81) — creates the book PDA (`[b"market_book", market]`),
   ≤ 10,240 B so it is one-CPI delegate-safe.
5. `update_oracle` (Ix 15) — set a non-zero mark (the place/active checks need it).

(These can be scripted with the same raw-instruction style as `er_acceptance_pin.mjs`.)

## Run

Gated on `ER_RPC` / `PIN_PROGRAM_ID` / `MARKET` — skips cleanly (exit 0) when unset,
so it never breaks CI.

```bash
npm install
L1_RPC=https://api.devnet.solana.com \
ER_RPC=https://devnet-as.magicblock.app \
PIN_PROGRAM_ID=<deployed pin program id> \
MARKET=<initialized, active, mark-set market pubkey> \
  npm run acceptance
```

| env | meaning | default |
|-----|---------|---------|
| `ER_RPC` | the ER validator endpoint the accounts delegate to | — (required) |
| `PIN_PROGRAM_ID` | the deployed `flash-book-pin` program id | — (required) |
| `MARKET` | an initialized, active, mark-set market (book init'd) | — (required) |
| `L1_RPC` | base-layer RPC | `https://api.devnet.solana.com` |
| `ER_VALIDATOR` | the validator pubkey pinned at delegate time | `MAS1Dt9…` |
| `KEYPAIR` | path to the payer/authority keypair | `~/.config/solana/id.json` |

The keypair must be the **market authority** (it signs `delegate_market_book`).

## Expected output

```
live-ER acceptance (pin) — L1=… ER=…
  ✓ L1 precheck: book exists and is program-owned
  ✓ L1 delegate_market_book → DLP (WAVE-24i staging)
  ✓ ER place_limit_order (rest a bid on the delegated book)
  ✓ ER commit_market_book → L1 snapshot
  ✓ L1 assert book is delegated (owned by the DLP)
  ✓ ER commit_and_undelegate_market_book → L1 finalize
  ✓ L1 assert book back program-owned + non-empty (validate_node_links accepted)

live-ER acceptance (pin): 7/7 stages green
```

A green run is the live confirmation that #190's `cpi_delegate` staging + the
undelegate path work against the real MagicBlock DLP.
