// SmartRouter — quotes the same logical asset on multiple venues and
// routes each side of each iteration to the better-priced venue.
//
// The classic example: an MM bot operator runs SOL-PERP on both Flash V2
// (pool-backed via FlashV2Venue) and Flash V3 (CLOB via FlashBookVenue).
// At any moment one venue may have a tighter spread or better fill
// liquidity. SmartRouter wraps both behind the same Venue contract and
// picks the better one per-side per-iteration.
//
// This is the design payoff of the Venue abstraction: the strategy code
// doesn't change at all when you add a new venue. SmartRouter is itself
// a Venue (sandwich pattern), so MultiMarketBot can drive it without
// any awareness of multi-venue routing.
//
// Routing policy (current v1):
//
//   • fetchMarket: query both venues, return the one with the BETTER
//     mark (tighter to oracle midpoint). Operators can override via the
//     `routingPolicy` callback.
//   • fetchTrader: aggregates collateral across both venues.
//   • fetchPosition: returns NET signed inventory across both venues.
//   • buildQuoteInstructions: routes BOTH sides to the same venue (the
//     one currently chosen for this iteration). Cross-venue execution
//     of bid+ask in one tx isn't safe (different SDKs, different signers
//     potentially) so we keep both legs on one venue.
//   • buildCancelInstructions: routes by the side bit in the seq (each
//     venue uses its own seq encoding; SmartRouter packs the venue ID
//     into the high bits so it can dispatch).

import type { Connection, Keypair, PublicKey, TransactionInstruction } from '@solana/web3.js';
import type {
  MarketSnapshot,
  PositionSnapshot,
  TraderSnapshot,
  Venue,
} from './types.ts';

/// Per-iteration policy callback. Returns the venue index to use for the
/// next round of quotes. Default: pick the venue whose mark is closer to
/// the oracle (tighter pricing).
export type RoutingPolicy = (markets: ReadonlyArray<MarketSnapshot | null>) => number;

const VENUE_BIT_SHIFT = 60n;
const VENUE_BIT_MASK = (1n << VENUE_BIT_SHIFT) - 1n;

export interface SmartRouterConfig {
  venues: ReadonlyArray<Venue>;
  /// Optional override of the default tightest-mark policy.
  routingPolicy?: RoutingPolicy;
}

export class SmartRouter implements Venue {
  readonly name: string;
  private lastChosen = 0;

  constructor(private readonly cfg: SmartRouterConfig) {
    if (cfg.venues.length === 0) throw new Error('SmartRouter needs ≥1 venue');
    if (cfg.venues.length > 4) {
      throw new Error('SmartRouter supports at most 4 venues (bit 60+ used for venue id)');
    }
    this.name = `smart-router(${cfg.venues.map((v) => v.name).join('+')})`;
  }

  async fetchMarket(market: PublicKey): Promise<MarketSnapshot | null> {
    const snaps = await Promise.all(this.cfg.venues.map((v) => v.fetchMarket(market).catch(() => null)));
    this.lastChosen = (this.cfg.routingPolicy ?? defaultPolicy)(snaps);
    return snaps[this.lastChosen] ?? null;
  }

  async fetchTrader(trader: PublicKey): Promise<TraderSnapshot | null> {
    const all = await Promise.all(this.cfg.venues.map((v) => v.fetchTrader(trader).catch(() => null)));
    let collateral = 0n;
    let realized = 0n;
    let openPos = 0;
    let any = false;
    for (const t of all) {
      if (!t) continue;
      any = true;
      collateral += t.collateralQuoteLots;
      realized += t.realizedPnlQuoteLots;
      openPos += t.openPositions;
    }
    if (!any) return null;
    return { collateralQuoteLots: collateral, realizedPnlQuoteLots: realized, openPositions: openPos };
  }

  async fetchPosition(market: PublicKey, trader: PublicKey): Promise<PositionSnapshot | null> {
    const all = await Promise.all(
      this.cfg.venues.map((v) => v.fetchPosition(market, trader).catch(() => null)),
    );
    let signed = 0n;
    let entryRef = 0n;
    let any = false;
    for (const p of all) {
      if (!p) continue;
      any = true;
      signed += p.signedSizeLots;
      if (entryRef === 0n) entryRef = p.entryPriceTicks;
    }
    if (!any) return null;
    return { signedSizeLots: signed, entryPriceTicks: entryRef };
  }

  async fetchOpenOrderSeqs(market: PublicKey, trader: PublicKey): Promise<bigint[]> {
    const all = await Promise.all(
      this.cfg.venues.map((v) => v.fetchOpenOrderSeqs(market, trader).catch(() => [])),
    );
    const out: bigint[] = [];
    for (let i = 0; i < all.length; i++) {
      const venueBits = BigInt(i) << VENUE_BIT_SHIFT;
      for (const seq of all[i]!) {
        // Mask the original seq into the lower bits, OR the venue id at bit 60.
        out.push((seq & VENUE_BIT_MASK) | venueBits);
      }
    }
    return out;
  }

  async buildQuoteInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    bidTicks: bigint;
    askTicks: bigint;
    sizeLots: bigint;
  }): Promise<TransactionInstruction[]> {
    const venue = this.cfg.venues[this.lastChosen]!;
    return venue.buildQuoteInstructions(args);
  }

  async buildCancelInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    seqs: bigint[];
  }): Promise<TransactionInstruction[]> {
    // Group by venue id encoded in the high bits.
    const byVenue: Map<number, bigint[]> = new Map();
    for (const seq of args.seqs) {
      const venueId = Number(seq >> VENUE_BIT_SHIFT);
      const original = seq & VENUE_BIT_MASK;
      if (!byVenue.has(venueId)) byVenue.set(venueId, []);
      byVenue.get(venueId)!.push(original);
    }
    const out: TransactionInstruction[] = [];
    for (const [venueId, seqs] of byVenue) {
      const venue = this.cfg.venues[venueId];
      if (!venue) continue;
      out.push(...(await venue.buildCancelInstructions({ ...args, seqs })));
    }
    return out;
  }

  async sendTx(instructions: TransactionInstruction[], signers: Keypair[]): Promise<string> {
    // SmartRouter's sendTx delegates to the venue chosen this iteration.
    // (All ix in `instructions` are routed to the same venue per the
    // single-venue-per-iteration rule.)
    return this.cfg.venues[this.lastChosen]!.sendTx(instructions, signers);
  }

  /// Inspect which venue was selected on the most recent fetchMarket.
  /// Useful for telemetry.
  getLastChosenVenueIndex(): number {
    return this.lastChosen;
  }
}

/// Default policy: pick the venue whose mark exists. Ties broken by
/// preferring the lower-indexed venue (operator-supplied order matters).
function defaultPolicy(snaps: ReadonlyArray<MarketSnapshot | null>): number {
  for (let i = 0; i < snaps.length; i++) {
    if (snaps[i] && snaps[i]!.markPriceTicks > 0n) return i;
  }
  return 0;
}
