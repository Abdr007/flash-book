# Running the on-validator exercise against **devnet** (real, live transactions)

`tests/local_exercise.rs` is RPC-agnostic. Pointed at a local `solana-test-validator`
it proves the program offline (133/133); pointed at **devnet** with a funded wallet it
fires the **same instructions as real on-chain transactions** and prints a clickable
`explorer.solana.com` link for every one.

> Why this isn't run from the build sandbox: that environment has no outbound network
> (devnet/MagicBlock unreachable), so the live devnet run must happen from a machine
> with internet. The harness was written so that machine is the only thing it needs.

## 1. Fund a devnet wallet (~10 SOL)

```bash
solana-keygen new -o ~/devnet-payer.json            # or reuse a wallet
solana config set --url https://api.devnet.solana.com
solana airdrop 2 ~/devnet-payer.json                # repeat a few times (≈10 SOL total)
solana balance ~/devnet-payer.json
```

This wallet becomes the protocol authority + mint authority + sequencer, and **funds
every other account in the run** (so devnet's airdrop limits don't matter).

## 2. Build + deploy the pin program to devnet

```bash
cargo build-sbf --manifest-path programs/flash-book-pin/Cargo.toml --tools-version v1.52
solana program deploy \
  --url https://api.devnet.solana.com \
  --keypair ~/devnet-payer.json \
  --program-id programs/flash-book-pin/target/deploy/flash_book_pin-keypair.json \
  programs/flash-book-pin/target/deploy/flash_book_pin.so
# → prints the Program Id (default keypair gives CmCQDqY4fZL5nqCMZfy7GanUAH6qwhWjgqGfRx9LSqjo)
```

## 3. Run the exercise against devnet

```bash
cd programs/flash-book-pin
LOCAL_EXERCISE=1 \
RPC_URL=https://api.devnet.solana.com \
PIN_PROGRAM_ID=<the program id from step 2> \
KEYPAIR=~/devnet-payer.json \
  cargo test --test local_exercise full_lifecycle -- --nocapture --test-threads=1
```

Output (every PASS is a real devnet signature with an explorer link):

```
  PASS  init_insurance_fund
        https://explorer.solana.com/tx/2Yk8Vkw9…?cluster=devnet
  PASS  initialize_market
        https://explorer.solana.com/tx/42CsSVmU…?cluster=devnet
  …
  133/133 instructions passed on the live validator
```

Notes:
- Each run uses **fresh keypairs** for the market/trader accounts, so it does **not**
  collide with a previous devnet run (no `--reset` needed, unlike local). The only
  singletons are the insurance fund + FLP exposure PDAs — if a prior run already
  created them under this program id, re-deploy to a fresh program id or skip those two.
- It takes a few minutes: there are two ~60-slot FLP min-hold waits and a haircut
  maturation wait, all real on-chain time.
- Every printed signature is independently verifiable: open the explorer link, or
  `solana confirm -v <sig> --url devnet`.

## ER (MagicBlock) instructions

The ER `delegate`/`commit`/`undelegate` round-trip is a **separate** suite —
`../er-acceptance-pin/` — because it CPIs into the live MagicBlock delegation program.
See that folder's README; run with `ER_RPC=https://devnet-as.magicblock.app`.
