// Commit-reveal protocol for sequencer-proof MEV resistance.
//
// Phase 1 (block N):       trader submits hash(side ‖ size ‖ limit ‖ nonce ‖ trader)
// Phase 2 (block ≤ N + K): trader reveals all parts; matcher checks hash
// Phase 3 (next batch):    revealed order enters the FBA buffer
//
// Sequencer cannot front-run because the hash hides every value. To prevent
// commit spam (commit-without-reveal grief), each commit posts a small bond
// that is seized if no valid reveal lands within K batches.
//
// Censorship resistance: a reveal can also be force-included on L1; on the
// next ER session sync the matcher honors it with the original commit timestamp
// for ordering purposes.

import { commitHash } from './math.ts';
import type { CommitEntry, Order, Side } from './types.ts';

export interface RevealPayload {
  readonly market: string;
  readonly trader: string;
  readonly side: Side;
  readonly size: number;
  readonly limitPrice: number;
  readonly nonce: string;
}

export function buildCommitHash(payload: RevealPayload): string {
  return commitHash([
    payload.market,
    payload.trader,
    payload.side,
    payload.size.toString(),
    payload.limitPrice.toString(),
    payload.nonce,
  ]);
}

export class CommitRevealRegistry {
  private readonly commits = new Map<string, CommitEntry>();
  private readonly seizedBonds: Array<{ hash: string; trader: string; bond: number; batchNum: number }> = [];

  registerCommit(args: {
    hash: string;
    trader: string;
    market: string;
    bondLamports: number;
    currentBatch: number;
    expireInBatches: number;
  }): void {
    if (this.commits.has(args.hash)) {
      throw new Error(`Commit already exists for hash ${args.hash}`);
    }
    this.commits.set(args.hash, {
      hash: args.hash,
      trader: args.trader,
      market: args.market,
      bondLamports: args.bondLamports,
      committedAtBatch: args.currentBatch,
      expireAtBatch: args.currentBatch + args.expireInBatches,
    });
  }

  /**
   * Verify a reveal against the prior commit. If valid, return a synthesized
   * taker order to enqueue into the next batch and remove the commit.
   *
   * Returns null if the reveal does not match a committed hash.
   */
  redeem(args: {
    payload: RevealPayload;
    currentBatch: number;
    nowMs: number;
    orderIdSeq: number;
  }): Order | null {
    const expectedHash = buildCommitHash(args.payload);
    const entry = this.commits.get(expectedHash);
    if (!entry) return null;
    if (entry.trader !== args.payload.trader) return null;
    if (entry.market !== args.payload.market) return null;
    if (args.currentBatch > entry.expireAtBatch) {
      // Reveal arrived too late; commit will be forfeited on next sweep.
      return null;
    }
    this.commits.delete(expectedHash);
    return {
      id: `reveal_${args.orderIdSeq}_b${args.currentBatch}`,
      market: args.payload.market,
      trader: args.payload.trader,
      side: args.payload.side,
      size: args.payload.size,
      limitPrice: args.payload.limitPrice,
      type: 'taker',
      timestamp: args.nowMs,
      postOnly: false,
    };
  }

  /** Sweep expired commits and seize bonds. */
  sweepExpired(currentBatch: number): Array<{ hash: string; trader: string; bond: number }> {
    const seized: Array<{ hash: string; trader: string; bond: number }> = [];
    for (const [hash, entry] of this.commits) {
      if (currentBatch > entry.expireAtBatch) {
        seized.push({ hash, trader: entry.trader, bond: entry.bondLamports });
        this.seizedBonds.push({
          hash,
          trader: entry.trader,
          bond: entry.bondLamports,
          batchNum: currentBatch,
        });
        this.commits.delete(hash);
      }
    }
    return seized;
  }

  pendingCount(): number {
    return this.commits.size;
  }

  totalSeizedBonds(): number {
    let total = 0;
    for (const s of this.seizedBonds) total += s.bond;
    return total;
  }
}
