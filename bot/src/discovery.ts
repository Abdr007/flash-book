// Auto-discovery for keepers — uses `getProgramAccounts` to find every
// PositionAccount / TraderStateAccount on chain, then filters to actionable
// targets. Removes the need for operators to maintain an external indexer.
//
// Three discovery modes:
//
//   • discoverActivePositions — every Position with size_lots > 0.
//     LiquidationKeeper consumes this to know whom to evaluate.
//
//   • discoverPositionsByMarket — same as above but filtered to a single
//     market via memcmp. Cheaper RPC when only one market is monitored.
//
//   • discoverEmptyTraderAtas — TraderStates with zero collateral and
//     zero open positions. AtaCleanupKeeper consumes this to find ATAs
//     to close.
//
// All three return PublicKeys + the parsed account data so the keeper
// can immediately evaluate without a second RPC. Discovery is BANDWIDTH-
// HEAVY (full account scan); operators should run it on a slow cadence
// (e.g. every 5 min) and cache the watchlist between scans.

import { Connection, PublicKey } from '@solana/web3.js';
import {
  decodeAccount,
  type PositionAccount,
  type TraderStateAccount,
} from '../../sdk-ts/src/accounts.ts';
import { FLASH_BOOK_PROGRAM_ID } from '../../sdk-ts/src/pdas.ts';

/// PositionAccount Anchor discriminator (sha256("account:PositionAccount")[..8]).
/// Pulled from the IDL by the BorshAccountsCoder; we recompute it inline
/// for memcmp filters since `getProgramAccounts` doesn't accept an IDL.
const POSITION_DISCRIMINATOR = computeAnchorDiscriminator('PositionAccount');
const TRADER_STATE_DISCRIMINATOR = computeAnchorDiscriminator('TraderStateAccount');

function computeAnchorDiscriminator(name: string): Buffer {
  // Anchor's account discriminator: first 8 bytes of sha256("account:" + name).
  // We avoid a node:crypto dep by using Web Crypto if available, or a
  // tiny inline SHA256 — but in practice this code runs in node/bun so
  // we use the runtime's built-in.
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { createHash } = require('node:crypto');
  return createHash('sha256').update(`account:${name}`).digest().subarray(0, 8);
}

export interface DiscoveredPosition {
  address: PublicKey;
  account: PositionAccount;
}

export interface DiscoveredTraderState {
  address: PublicKey;
  account: TraderStateAccount;
}

export interface DiscoveryConfig {
  programId?: PublicKey;
  /// Optional: filter discovery to a single market. Speeds up scans
  /// when only one market is monitored.
  marketFilter?: PublicKey;
  /// Optional: minimum position size to include (in base lots). Skips
  /// dust positions that aren't worth a liquidation tx fee.
  minSizeLots?: bigint;
}

/// Discover every active (size_lots > 0) PositionAccount on chain.
/// Returns parsed accounts ready for keeper consumption.
export async function discoverActivePositions(
  connection: Connection,
  cfg: DiscoveryConfig = {},
): Promise<DiscoveredPosition[]> {
  const programId = cfg.programId ?? FLASH_BOOK_PROGRAM_ID;
  const filters: { memcmp: { offset: number; bytes: string } }[] = [
    { memcmp: { offset: 0, bytes: bs58Encode(POSITION_DISCRIMINATOR) } },
  ];
  if (cfg.marketFilter) {
    // PositionAccount layout: 8 (disc) + 32 (trader) = market starts at 40.
    filters.push({
      memcmp: { offset: 40, bytes: cfg.marketFilter.toBase58() },
    });
  }
  const raw = await connection.getProgramAccounts(programId, {
    filters,
    commitment: 'confirmed',
  });
  const minSize = cfg.minSizeLots ?? 1n;
  const out: DiscoveredPosition[] = [];
  for (const r of raw) {
    try {
      const acc = decodeAccount<PositionAccount>('positionAccount', r.account.data as Buffer);
      const sizeBN = acc.sizeLots;
      if (BigInt(sizeBN.toString()) >= minSize) {
        out.push({ address: r.pubkey, account: acc });
      }
    } catch {
      // Skip malformed entries — discriminator filter doesn't guarantee
      // perfect parsing if older variants exist.
    }
  }
  return out;
}

/// Discover TraderStateAccounts with zero collateral and zero open
/// positions — candidates for ATA cleanup.
export async function discoverEmptyTraderStates(
  connection: Connection,
  cfg: DiscoveryConfig = {},
): Promise<DiscoveredTraderState[]> {
  const programId = cfg.programId ?? FLASH_BOOK_PROGRAM_ID;
  const filters: { memcmp: { offset: number; bytes: string } }[] = [
    { memcmp: { offset: 0, bytes: bs58Encode(TRADER_STATE_DISCRIMINATOR) } },
  ];
  const raw = await connection.getProgramAccounts(programId, {
    filters,
    commitment: 'confirmed',
  });
  const out: DiscoveredTraderState[] = [];
  for (const r of raw) {
    try {
      const acc = decodeAccount<TraderStateAccount>('traderStateAccount', r.account.data as Buffer);
      if (acc.openPositions === 0 && BigInt(acc.collateralQuoteLots.toString()) === 0n) {
        out.push({ address: r.pubkey, account: acc });
      }
    } catch {
      // skip
    }
  }
  return out;
}

/// Tiny base58 encoder — avoids pulling bs58 as a dep for one call.
/// Used for Anchor discriminator memcmp filters in getProgramAccounts.
function bs58Encode(buf: Buffer): string {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const bs58 = require('bs58');
  // bs58 v5 exports default, v4 exports the function directly. Support both.
  const enc = (bs58.default ?? bs58) as { encode: (b: Uint8Array) => string };
  return enc.encode(new Uint8Array(buf));
}
