// Market-maker reference bot for Flash Book and (via venue adapter) Flash V2.
//
// Strategy: Avellaneda-Stoikov inventory-aware quoting + VPIN-scaled spread
// widening, with a kill switch on max drawdown and a hard inventory cap.
//
// Architecture: the strategy itself is venue-agnostic — it consumes a
// `MarketSnapshot` + `TraderSnapshot` and emits a quote pair. A `Venue`
// adapter handles fetching/placing/cancelling. We ship a `FlashBookVenue`
// today; a `FlashV2Venue` can plug in alongside it as long as it exposes
// the same shape. This is the design contract for "compatible with Flash
// SDK v2": same strategy, swap the adapter.
//
// The adversarial design choice is deliberate: by abstracting the venue,
// the strategy code can be audited once and trusted across both backends.
// Risk parameters live in one place, so tuning a position cap automatically
// applies to both V2 and V3 if you run a hybrid quoter.

import {
  Connection,
  Keypair,
  PublicKey,
  type TransactionInstruction,
} from '@solana/web3.js';
import { FlashBookClient } from '../../sdk-ts/src/client.ts';
import {
  fetchMarket,
  fetchOrderBuffer,
  fetchPosition,
  fetchTraderState,
  type MarketAccount,
  type OrderSlot,
  type PositionAccount,
  type TraderStateAccount,
} from '../../sdk-ts/src/accounts.ts';

// ─── Shared snapshot types ────────────────────────────────────────────

/// Market state the strategy needs. Venue-agnostic.
export interface MarketSnapshot {
  /// Mark price in ticks.
  markPriceTicks: bigint;
  /// VPIN as bps (0..10_000). Toxicity widening signal.
  vpinBps: number;
  /// On-chain tick size in quote lots per tick.
  tickSize: bigint;
  /// On-chain min order size in base lots.
  minBaseLots: bigint;
  /// Open interest balance for OI-skew adjustment (signed: +long, -short).
  oiImbalanceLots: bigint;
  /// Total OI for normalization.
  oiTotalLots: bigint;
  /// Current batch number on the venue (for re-quote cadence).
  currentBatch: bigint;
}

/// Trader state the strategy needs.
export interface TraderSnapshot {
  /// Free collateral in quote lots.
  collateralQuoteLots: bigint;
  /// Cumulative realized PnL since session start.
  realizedPnlQuoteLots: bigint;
  /// Number of open positions (any market).
  openPositions: number;
}

/// Per-market position snapshot.
export interface PositionSnapshot {
  /// Signed position size in base lots: positive = long, negative = short.
  signedSizeLots: bigint;
  /// Average entry price in ticks.
  entryPriceTicks: bigint;
}

// ─── Venue adapter contract ───────────────────────────────────────────

/// Pluggable venue. Strategy depends only on this trait, so adding a Flash
/// V2 backend is purely a matter of implementing it against the V2 SDK.
export interface Venue {
  /// Stable identifier shown in logs.
  readonly name: string;
  fetchMarket(market: PublicKey): Promise<MarketSnapshot | null>;
  fetchTrader(trader: PublicKey): Promise<TraderSnapshot | null>;
  fetchPosition(market: PublicKey, trader: PublicKey): Promise<PositionSnapshot | null>;
  /// Returns the seqs of OUR open orders in the buffer (for cancellation).
  fetchOpenOrderSeqs(market: PublicKey, trader: PublicKey): Promise<bigint[]>;
  /// Build the bid + ask placement instructions. Caller signs and sends.
  buildQuoteInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    bidTicks: bigint;
    askTicks: bigint;
    sizeLots: bigint;
  }): Promise<TransactionInstruction[]>;
  /// Build cancellation instructions for the given seqs.
  buildCancelInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    seqs: bigint[];
  }): Promise<TransactionInstruction[]>;
  /// Send a built tx (the venue knows its own RPC + wallet).
  sendTx(instructions: TransactionInstruction[], signers: Keypair[]): Promise<string>;
}

// ─── Quote math (pure, testable) ──────────────────────────────────────

export interface QuoteParams {
  /// Base half-spread in bps (per side, off fair value).
  baseSpreadBps: number;
  /// Spread widening per VPIN unit. e.g. 0.5 means +50% spread at vpin=1.0.
  vpinSpreadAlpha: number;
  /// Skew per inventory unit. inventory_fraction × this = skew bps applied
  /// to fair value (negative if we're long, positive if we're short — pulls
  /// fair down to make us a more attractive ask).
  inventorySkewBpsPerUnit: number;
  /// Spread widening proportional to OI imbalance magnitude.
  oiImbalanceSpreadCoef: number;
  /// Quote size per side in base lots.
  quoteSizeLots: bigint;
}

export interface QuoteOutput {
  bidTicks: bigint;
  askTicks: bigint;
  fairValueTicks: bigint;
  effectiveSpreadBps: number;
  /// True if neither side is quotable (e.g. inventory cap blocks all sides
  /// or fair value is non-positive).
  empty: boolean;
}

/// Compute a single bid/ask quote pair around the venue's mark price,
/// adjusted for inventory and toxicity. Returns ticks aligned to the
/// market's tick size.
export function computeQuote(args: {
  market: MarketSnapshot;
  inventorySignedLots: bigint;
  capitalQuoteLots: bigint;
  params: QuoteParams;
  /// If true, skip the bid (we're at +inventory cap).
  skipBid?: boolean;
  /// If true, skip the ask (we're at -inventory cap).
  skipAsk?: boolean;
}): QuoteOutput {
  const { market, inventorySignedLots, capitalQuoteLots, params } = args;
  const empty: QuoteOutput = {
    bidTicks: 0n,
    askTicks: 0n,
    fairValueTicks: market.markPriceTicks,
    effectiveSpreadBps: 0,
    empty: true,
  };

  if (market.markPriceTicks <= 0n || capitalQuoteLots <= 0n) return empty;

  // Inventory fraction: dimensionless. Used to skew fair-value away from
  // the side we want to attract.
  // notional = |inventory| × markPrice × tickSize, all in quote-lot terms.
  // capital is in quote lots; ratio is signed inventory_notional / capital.
  const invMag = inventorySignedLots < 0n ? -inventorySignedLots : inventorySignedLots;
  const invNotional = invMag * market.markPriceTicks * market.tickSize;
  // inventoryFractionBps: bps of capital. Signed.
  const sign = inventorySignedLots < 0n ? -1 : 1;
  const invFractionBps =
    capitalQuoteLots > 0n
      ? Number((invNotional * 10_000n) / capitalQuoteLots) * sign
      : 0;
  const inventorySkewBps = -params.inventorySkewBpsPerUnit * (invFractionBps / 10_000);

  // OI imbalance — widens spread, doesn't skew (Glosten-Milgrom-style).
  const oiImbalanceMag =
    market.oiTotalLots > 0n
      ? Math.abs(Number(market.oiImbalanceLots) / Number(market.oiTotalLots))
      : 0;

  // Effective spread (bps), per side.
  const vpinFraction = market.vpinBps / 10_000;
  const halfSpreadBps =
    params.baseSpreadBps +
    params.vpinSpreadAlpha * vpinFraction * 10_000 +
    params.oiImbalanceSpreadCoef * oiImbalanceMag * 10_000;

  // Apply skew first to derive fair value, then build bid/ask off fair.
  const mark = Number(market.markPriceTicks);
  const fair = mark * (1 + inventorySkewBps / 10_000);
  const fairTicks = BigInt(Math.round(fair));
  const halfSpreadFraction = halfSpreadBps / 10_000;
  const bidPx = fair * (1 - halfSpreadFraction);
  const askPx = fair * (1 + halfSpreadFraction);

  // Round to tick size.
  const tick = market.tickSize;
  const tickN = Number(tick);
  const bidAlignedTicks = BigInt(Math.floor(bidPx / tickN)) * tick;
  const askAlignedTicks = BigInt(Math.ceil(askPx / tickN)) * tick;

  const bidValid = bidAlignedTicks > 0n && !args.skipBid;
  const askValid = askAlignedTicks > 0n && !args.skipAsk;
  if (!bidValid && !askValid) return empty;

  return {
    bidTicks: bidValid ? bidAlignedTicks : 0n,
    askTicks: askValid ? askAlignedTicks : 0n,
    fairValueTicks: fairTicks,
    effectiveSpreadBps: halfSpreadBps * 2,
    empty: false,
  };
}

// ─── Risk gates ───────────────────────────────────────────────────────

export interface RiskLimits {
  /// Hard cap on absolute inventory in base lots. Quote-side blocked when
  /// signed inventory hits the bound on that side.
  maxInventoryLots: bigint;
  /// Daily realized drawdown threshold; bot enters kill-switch state if
  /// session realized PnL drops below this (negative number expected).
  maxDrawdownQuoteLots: bigint;
  /// Hard floor on free collateral. Below this the bot stops opening new
  /// quotes and only reduces inventory.
  minCollateralQuoteLots: bigint;
}

export interface RiskGateOutput {
  /// True if the bot is allowed to quote at all this iteration.
  canQuote: boolean;
  /// True if the bid side should be skipped (would exceed +inventory cap).
  skipBid: boolean;
  /// True if the ask side should be skipped (would exceed -inventory cap).
  skipAsk: boolean;
  /// True if the kill switch has tripped — bot enters wind-down mode.
  killSwitchActive: boolean;
  /// Human-readable reason if !canQuote.
  reason?: string | undefined;
}

export function checkRiskGates(args: {
  inventorySignedLots: bigint;
  collateralQuoteLots: bigint;
  realizedPnlQuoteLots: bigint;
  limits: RiskLimits;
  quoteSizeLots: bigint;
}): RiskGateOutput {
  const { limits } = args;

  if (args.realizedPnlQuoteLots <= limits.maxDrawdownQuoteLots) {
    return {
      canQuote: false,
      skipBid: true,
      skipAsk: true,
      killSwitchActive: true,
      reason: `kill switch: realized PnL ${args.realizedPnlQuoteLots} ≤ drawdown limit ${limits.maxDrawdownQuoteLots}`,
    };
  }

  if (args.collateralQuoteLots < limits.minCollateralQuoteLots) {
    return {
      canQuote: false,
      skipBid: true,
      skipAsk: true,
      killSwitchActive: false,
      reason: `collateral ${args.collateralQuoteLots} < floor ${limits.minCollateralQuoteLots}`,
    };
  }

  // Inventory-side caps. If we'd add to the cap by filling on a side,
  // skip that side this round.
  const projectedLong = args.inventorySignedLots + args.quoteSizeLots;
  const projectedShort = args.inventorySignedLots - args.quoteSizeLots;
  const skipBid = projectedLong > limits.maxInventoryLots;
  const skipAsk = -projectedShort > limits.maxInventoryLots;

  return {
    canQuote: !(skipBid && skipAsk),
    skipBid,
    skipAsk,
    killSwitchActive: false,
    reason: skipBid && skipAsk ? 'inventory cap hit on both sides' : undefined,
  };
}

// ─── Bot config + stats ───────────────────────────────────────────────

export interface MarketMakerConfig {
  market: PublicKey;
  trader: PublicKey;
  signer: Keypair;
  quoteParams: QuoteParams;
  riskLimits: RiskLimits;
  /// Re-quote cadence (ms). Should be ≥ market.batchIntervalMs.
  quoteRefreshMs: number;
  /// If true, compute quotes + log but never send a tx.
  dryRun: boolean;
}

export interface MarketMakerStats {
  startedAt: number;
  iterationsCompleted: number;
  ordersPlaced: number;
  ordersCancelled: number;
  txErrors: number;
  killSwitchActive: boolean;
  lastQuote: QuoteOutput | null;
  lastInventory: bigint;
  lastRealizedPnl: bigint;
  lastError?: string | undefined;
}

// ─── Main bot ─────────────────────────────────────────────────────────

export class MarketMaker {
  private readonly stats: MarketMakerStats;
  private timer: ReturnType<typeof setInterval> | null = null;
  private busy = false;

  constructor(
    private readonly venue: Venue,
    private readonly config: MarketMakerConfig,
  ) {
    this.stats = {
      startedAt: Date.now(),
      iterationsCompleted: 0,
      ordersPlaced: 0,
      ordersCancelled: 0,
      txErrors: 0,
      killSwitchActive: false,
      lastQuote: null,
      lastInventory: 0n,
      lastRealizedPnl: 0n,
    };
  }

  /// Start the quote loop. Resolves immediately; loop runs in background.
  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => {
      // Skip if previous iteration still in flight (avoid stepping on tx).
      if (this.busy) return;
      void this.iterate();
    }, this.config.quoteRefreshMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  getStats(): Readonly<MarketMakerStats> {
    return { ...this.stats };
  }

  /// One quote cycle. Public for testability — call from outside to step.
  async iterate(): Promise<void> {
    this.busy = true;
    try {
      const market = await this.venue.fetchMarket(this.config.market);
      if (!market) {
        this.stats.lastError = 'market account not found';
        return;
      }
      const trader = await this.venue.fetchTrader(this.config.trader);
      if (!trader) {
        this.stats.lastError = 'trader state not found';
        return;
      }
      const position = await this.venue.fetchPosition(
        this.config.market,
        this.config.trader,
      );
      const inventory = position?.signedSizeLots ?? 0n;

      this.stats.lastInventory = inventory;
      this.stats.lastRealizedPnl = trader.realizedPnlQuoteLots;

      const gates = checkRiskGates({
        inventorySignedLots: inventory,
        collateralQuoteLots: trader.collateralQuoteLots,
        realizedPnlQuoteLots: trader.realizedPnlQuoteLots,
        limits: this.config.riskLimits,
        quoteSizeLots: this.config.quoteParams.quoteSizeLots,
      });

      this.stats.killSwitchActive = gates.killSwitchActive;

      if (!gates.canQuote) {
        // Cancel any open quotes — wind down on risk trip.
        const seqs = await this.venue.fetchOpenOrderSeqs(
          this.config.market,
          this.config.trader,
        );
        if (seqs.length > 0 && !this.config.dryRun) {
          const ixs = await this.venue.buildCancelInstructions({
            trader: this.config.trader,
            market: this.config.market,
            seqs,
          });
          await this.venue.sendTx(ixs, [this.config.signer]);
          this.stats.ordersCancelled += seqs.length;
        }
        this.stats.lastError = gates.reason;
        return;
      }

      const quote = computeQuote({
        market,
        inventorySignedLots: inventory,
        capitalQuoteLots: trader.collateralQuoteLots,
        params: this.config.quoteParams,
        skipBid: gates.skipBid,
        skipAsk: gates.skipAsk,
      });
      this.stats.lastQuote = quote;
      if (quote.empty) {
        this.stats.lastError = 'quote computed empty';
        return;
      }

      // Cancel-then-replace cycle. Future enhancement: leave orders in place
      // when prices haven't moved past a threshold (avoid unnecessary
      // cancel+place tx fees). V1 always cancels.
      const seqs = await this.venue.fetchOpenOrderSeqs(
        this.config.market,
        this.config.trader,
      );
      const txIxs: TransactionInstruction[] = [];
      if (seqs.length > 0) {
        const cancelIxs = await this.venue.buildCancelInstructions({
          trader: this.config.trader,
          market: this.config.market,
          seqs,
        });
        txIxs.push(...cancelIxs);
      }
      const placeIxs = await this.venue.buildQuoteInstructions({
        trader: this.config.trader,
        market: this.config.market,
        bidTicks: quote.bidTicks,
        askTicks: quote.askTicks,
        sizeLots: this.config.quoteParams.quoteSizeLots,
      });
      txIxs.push(...placeIxs);

      if (this.config.dryRun) {
        this.stats.lastError = `dry-run: would send ${txIxs.length} ix`;
      } else if (txIxs.length > 0) {
        await this.venue.sendTx(txIxs, [this.config.signer]);
        this.stats.ordersCancelled += seqs.length;
        this.stats.ordersPlaced += placeIxs.length;
      }
      this.stats.lastError = undefined;
    } catch (e) {
      this.stats.txErrors += 1;
      this.stats.lastError = e instanceof Error ? e.message : String(e);
    } finally {
      this.stats.iterationsCompleted += 1;
      this.busy = false;
    }
  }
}

// ─── Flash Book V3 venue adapter ──────────────────────────────────────

/// Adapter that targets the Flash Book V3 program via FlashBookClient.
/// Sister adapter `FlashV2Venue` (not implemented here) would target the
/// pool engine via @flashtrade/perpetuals-sdk; same Venue contract.
export class FlashBookVenue implements Venue {
  readonly name = 'flash-book-v3';

  constructor(
    private readonly client: FlashBookClient,
    private readonly connection: Connection,
    private readonly quoteMint: PublicKey,
    private readonly quoteVault: PublicKey,
  ) {}

  async fetchMarket(market: PublicKey): Promise<MarketSnapshot | null> {
    const m = await fetchMarket(this.client, market);
    if (!m) return null;
    return marketAccountToSnapshot(m);
  }

  async fetchTrader(trader: PublicKey): Promise<TraderSnapshot | null> {
    const ts = await fetchTraderState(this.client, this.client.traderState(trader).address);
    if (!ts) return null;
    return traderStateToSnapshot(ts);
  }

  async fetchPosition(
    market: PublicKey,
    trader: PublicKey,
  ): Promise<PositionSnapshot | null> {
    const p = await fetchPosition(
      this.client,
      this.client.position(market, trader).address,
    );
    if (!p) return null;
    return positionToSnapshot(p);
  }

  async fetchOpenOrderSeqs(market: PublicKey, trader: PublicKey): Promise<bigint[]> {
    const buf = await fetchOrderBuffer(
      this.client,
      this.client.orderBuffer(market).address,
    );
    if (!buf) return [];
    const seqs: bigint[] = [];
    for (const slot of buf.slots as OrderSlot[]) {
      if (slot.valid !== 1) continue;
      if (!slot.trader.equals(trader)) continue;
      seqs.push(BigInt(slot.seq.toString()));
    }
    return seqs;
  }

  async buildQuoteInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    bidTicks: bigint;
    askTicks: bigint;
    sizeLots: bigint;
  }): Promise<TransactionInstruction[]> {
    const out: TransactionInstruction[] = [];
    if (args.bidTicks > 0n) {
      out.push(
        await this.client.placeLimitOrderIx({
          trader: args.trader,
          market: args.market,
          side: 'long',
          sizeLots: args.sizeLots,
          limitTicks: args.bidTicks,
          postOnly: true,
        }),
      );
    }
    if (args.askTicks > 0n) {
      out.push(
        await this.client.placeLimitOrderIx({
          trader: args.trader,
          market: args.market,
          side: 'short',
          sizeLots: args.sizeLots,
          limitTicks: args.askTicks,
          postOnly: true,
        }),
      );
    }
    return out;
  }

  async buildCancelInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    seqs: bigint[];
  }): Promise<TransactionInstruction[]> {
    const out: TransactionInstruction[] = [];
    for (const seq of args.seqs) {
      out.push(
        await this.client.cancelOrderIx({
          trader: args.trader,
          market: args.market,
          orderSeq: seq,
        }),
      );
    }
    return out;
  }

  async sendTx(instructions: TransactionInstruction[], signers: Keypair[]): Promise<string> {
    const { Transaction } = await import('@solana/web3.js');
    const tx = new Transaction().add(...instructions);
    tx.recentBlockhash = (await this.connection.getLatestBlockhash()).blockhash;
    tx.feePayer = signers[0]!.publicKey;
    tx.sign(...signers);
    const sig = await this.connection.sendRawTransaction(tx.serialize());
    await this.connection.confirmTransaction(sig, 'confirmed');
    return sig;
  }
}

// ─── Account → snapshot adapters ──────────────────────────────────────

function marketAccountToSnapshot(m: MarketAccount): MarketSnapshot {
  // VPIN as_bps mirrors the on-chain math: (value × 10_000) >> 32, capped.
  const valueQ = BigInt(m.vpin.valueQ32_32.toString());
  const vpinBpsBig = (valueQ * 10_000n) >> 32n;
  const vpinBps = Number(vpinBpsBig > 10_000n ? 10_000n : vpinBpsBig);

  const oiLong = BigInt(m.oiLongLots.toString());
  const oiShort = BigInt(m.oiShortLots.toString());

  return {
    markPriceTicks: BigInt(m.markPriceTicks.toString()),
    vpinBps,
    tickSize: BigInt(m.params.tickSize.toString()),
    minBaseLots: BigInt(m.params.minBaseLots.toString()),
    oiImbalanceLots: oiLong - oiShort,
    oiTotalLots: oiLong + oiShort,
    currentBatch: BigInt(m.currentBatch.toString()),
  };
}

function traderStateToSnapshot(t: TraderStateAccount): TraderSnapshot {
  return {
    collateralQuoteLots: BigInt(t.collateralQuoteLots.toString()),
    realizedPnlQuoteLots: BigInt(t.realizedPnlQuoteLots.toString()),
    openPositions: t.openPositions,
  };
}

function positionToSnapshot(p: PositionAccount): PositionSnapshot {
  // side: 0 = long → positive, 1 = short → negative.
  const sizeMag = BigInt(p.sizeLots.toString());
  const signed = p.side === 0 ? sizeMag : -sizeMag;
  return {
    signedSizeLots: signed,
    entryPriceTicks: BigInt(p.entryPriceTicks.toString()),
  };
}
