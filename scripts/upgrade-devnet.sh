#!/usr/bin/env bash
# Upgrade flash_book core on devnet.
# Verifies the program loads + a market_book PDA is readable post-upgrade.

set -euo pipefail

WALLET="$HOME/.config/solana/id.json"
PROGRAM_ID="5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq"
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
echo "▶ Smoke test — read the SOL/USDC market_book PDA from devnet"
echo ""

# Sanity check via the SDK that the program is healthy post-upgrade.
bun -e '
import { Connection, PublicKey } from "@solana/web3.js";
import { marketPda, marketBookPda } from "./sdk-ts/src/index.ts";

const RPC = "https://api.devnet.solana.com";
const SOL_MINT = new PublicKey("So11111111111111111111111111111111111111112");
const USDC = new PublicKey("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
const conn = new Connection(RPC, "confirmed");

const market = marketPda(SOL_MINT, USDC).address;
const book = marketBookPda(market).address;
console.log("  Market:     ", market.toBase58());
console.log("  MarketBook: ", book.toBase58());

const info = await conn.getAccountInfo(book);
if (!info) {
  console.log("  ✗ market_book account not found — bootstrap may be needed.");
  process.exit(1);
}
console.log("  ✓ market_book OK —", info.data.length, "bytes on devnet.");
'

echo ""
echo "✅ Devnet upgrade + smoke-test complete."
