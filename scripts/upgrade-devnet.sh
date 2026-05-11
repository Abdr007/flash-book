#!/usr/bin/env bash
# Upgrade flash_book core on devnet to the OOM-fixed build.
# Verifies the matcher tick doesn't panic post-upgrade.

set -euo pipefail

WALLET="$HOME/.config/solana/id.json"
PROGRAM_ID="HGP5GN7BHSt1geH1DxRwVGFg7g7ERU28Q2QEYf6KP24b"
PROGRAM_SO="target/deploy/flash_book.so"
PROGRAM_KP="target/deploy/flash_book-keypair.json"

echo "▶ Flash Book V3 — devnet program upgrade"
echo ""

# Check balance
BAL=$(solana balance --url devnet --keypair "$WALLET" 2>/dev/null | awk '{print $1}')
echo "  Wallet:       $(solana-keygen pubkey "$WALLET")"
echo "  Devnet bal:   $BAL SOL"
echo "  Need:         ~12 SOL for buffer (refunded after upgrade)"
echo ""

if (( $(echo "$BAL < 13" | bc -l) )); then
  echo "  ✗ Insufficient balance. Mine more with:"
  echo "    devnet-pow mine -d 3 --reward 0.02 --no-infer -t 12000000000 -u devnet"
  exit 1
fi

echo "▶ Upgrading flash_book on devnet (will take 1-3 min)..."
solana program deploy "$PROGRAM_SO" \
  --program-id "$PROGRAM_KP" \
  --url devnet \
  --keypair "$WALLET" 2>&1 | tail -5

echo ""
echo "▶ Smoke test — call run_batch_v2 on SOL/USDC market"
echo "  (empty book → no fills, but should NOT OOM)"
echo ""

# Run a quick matcher tick via the SDK to verify the fix landed.
bun -e '
import { Connection, Keypair, PublicKey, Transaction, ComputeBudgetProgram } from "@solana/web3.js";
import { AnchorProvider, BN, Wallet } from "@coral-xyz/anchor";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { FlashBookClient, marketPda } from "./sdk-ts/src/index.ts";

const RPC = "https://api.devnet.solana.com";
const SOL_MINT = new PublicKey("So11111111111111111111111111111111111111112");
const USDC = new PublicKey("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
const kp = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(path.join(os.homedir(), ".config/solana/id.json"), "utf8"))));
const conn = new Connection(RPC, "confirmed");
const client = new FlashBookClient(conn, new Wallet(kp));

const market = marketPda(SOL_MINT, USDC).address;
console.log("  Market:", market.toBase58());

const ix = await client.runBatchV2Ix({
  sequencer: kp.publicKey,
  market,
  nowMs: new BN(Date.now()),
});
const heapIx = ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 });
const cuIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 });

const tx = new Transaction().add(heapIx, cuIx, ix);
tx.feePayer = kp.publicKey;
tx.recentBlockhash = (await conn.getLatestBlockhash("confirmed")).blockhash;
tx.sign(kp);
const sig = await conn.sendRawTransaction(tx.serialize());
await conn.confirmTransaction(sig, "confirmed");
console.log("  ✓ run_batch_v2 succeeded on devnet:", sig);
console.log("  → OOM fix confirmed working on devnet.");
'

echo ""
echo "✅ Devnet upgrade + smoke-test complete."
