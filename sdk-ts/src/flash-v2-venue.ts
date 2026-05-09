// Flash V2 venue adapter — implements the Venue contract against the
// production Flash Trade Magic program via `@flash_trade/magic-trade-client`.
//
// Same MarketMaker strategy code drives both V2 (this file) and V3
// (FlashBookVenue). Swap by passing a different Venue to MarketMaker.
//
// V2 architecture notes (vs our V3 CLOB):
//   • Per-user single BasketAccount holds positions[] + orders[] across
//     ALL markets. No per-market trader_state PDA.
//   • Each (target, lock, collateral, side) tuple is a distinct MarketAccount.
//     Bid and ask are TWO markets (long-side and short-side), not one book.
//   • Limit orders live inside BasketAccount.orders, identified by orderId
//     (u8, 0..255 per user-side).
//   • Cancellation: editLimitOrder with both limit_price=0 AND size_amount=0
//     is the canonical cancel pattern (per IDL doc string).
//   • No FBA mark — V2 mark is the oracle. No VPIN signal exists, so the
//     strategy's VPIN-widening branch collapses to zero on V2 (intentional).
//
// We import the SDK dynamically inside method calls so consumers who don't
// use V2 don't pay the install cost. Type signatures use a structural
// `MagicTradeClient` interface to keep this file SDK-version-tolerant.

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  type TransactionInstruction,
  type Signer,
} from '@solana/web3.js';
import BN from 'bn.js';
import type {
  Venue,
  MarketSnapshot,
  TraderSnapshot,
  PositionSnapshot,
} from './market-maker.ts';

// ─── Types we mirror from @flash_trade/magic-trade-client ─────────────

/// Side variant in Anchor-variant form: `{ long: {} }` or `{ short: {} }`.
export type V2Side = { long: Record<string, never> } | { short: Record<string, never> };
export const V2_SIDE_LONG: V2Side = { long: {} };
export const V2_SIDE_SHORT: V2Side = { short: {} };

export interface V2OraclePrice {
  price: BN;
  exponent: number;
}

export interface V2PlaceLimitOrderParams {
  limitPrice: V2OraclePrice;
  collateralAmount: BN;
  sizeAmount: BN;
  stopLossPrice: V2OraclePrice;
  takeProfitPrice: V2OraclePrice;
}

export interface V2EditLimitOrderParams {
  orderId: number;
  limitPrice: V2OraclePrice;
  sizeAmount: BN;
  stopLossPrice: V2OraclePrice;
  takeProfitPrice: V2OraclePrice;
}

export interface V2InstructionResult {
  instructions: TransactionInstruction[];
  additionalSigners: Signer[];
}

/// Opaque PoolConfig — owned by the SDK; we only pass it through.
export interface V2PoolConfig {
  poolName?: string;
  // Many other fields owned by the SDK.
}

/// Subset of MagicTradePerpetualsClient we depend on.
export interface MagicTradeClient {
  placeLimitOrder(
    targetSymbol: string,
    collateralSymbol: string,
    side: V2Side,
    poolConfig: V2PoolConfig,
    params: V2PlaceLimitOrderParams,
    receivingSymbol?: string,
  ): Promise<V2InstructionResult>;

  editLimitOrder(
    targetSymbol: string,
    collateralSymbol: string,
    side: V2Side,
    poolConfig: V2PoolConfig,
    params: V2EditLimitOrderParams,
    receivingSymbol?: string,
  ): Promise<V2InstructionResult>;

  accounts: {
    fetchMarket(targetCustody: PublicKey, lockCustody: PublicKey, side: V2Side): Promise<V2MarketAccount>;
    fetchBasket(owner: PublicKey): Promise<V2BasketAccount>;
  };
}

/// Minimal V2 market shape.
export interface V2MarketAccount {
  collectivePosition: {
    sizeAmount: BN;
    sizeUsd: BN;
    averageEntryPrice: V2OraclePrice;
  };
}

/// Minimal V2 basket shape.
export interface V2BasketAccount {
  owner: PublicKey;
  positionsActive: boolean;
  ordersActive: boolean;
  positions: V2PositionMeta[];
  orders: V2OrderMeta[];
}

export interface V2PositionMeta {
  market: PublicKey;
  side: V2Side;
  sizeAmount: BN;
  collateralAmount: BN;
  entryPrice: V2OraclePrice;
  isActive: boolean;
}

export interface V2OrderMeta {
  market: PublicKey;
  orderId: number;
  side: V2Side;
  limitPrice: V2OraclePrice;
  sizeAmount: BN;
  isActive: boolean;
}

// ─── Adapter config ───────────────────────────────────────────────────

export interface FlashV2VenueConfig {
  poolConfig: V2PoolConfig;
  /// Target asset symbol (e.g. "SOL").
  targetSymbol: string;
  /// Collateral symbol (typically "USDC").
  collateralSymbol: string;
  /// Receiving symbol on settlement (defaults to USDC inside the SDK).
  receivingSymbol?: string;
  /// On-chain custody pubkeys (read from PoolConfig).
  targetCustody: PublicKey;
  /// Lock custody for long side (typically USDC).
  lockCustodyLong: PublicKey;
  /// Lock custody for short side (typically the target token).
  lockCustodyShort: PublicKey;
  /// Oracle price exponent (typically -8 for Pyth-style).
  priceExponent: number;
  /// Per-quote collateral amount in the smallest unit. If omitted, the
  /// venue uses the size amount as a conservative collateral bound.
  collateralPerQuote?: BN;
}

// ─── The adapter ──────────────────────────────────────────────────────

export class FlashV2Venue implements Venue {
  readonly name = 'flash-v2';

  constructor(
    private readonly client: MagicTradeClient,
    private readonly connection: Connection,
    private readonly cfg: FlashV2VenueConfig,
  ) {}

  async fetchMarket(_market: PublicKey): Promise<MarketSnapshot | null> {
    // V2 doesn't have a single canonical market account per asset — bid
    // and ask are split. We sample the LONG-side market for mark/stats.
    // Real adapter: the bot operator should wire a separate oracle reader
    // for live prices; this returns the most-recent collective entry as a
    // safe fallback so the strategy doesn't crash on first iteration.
    let mark = 0n;
    let oiNet = 0n;
    let oiTotal = 0n;
    try {
      const [longMkt, shortMkt] = await Promise.all([
        this.client.accounts.fetchMarket(this.cfg.targetCustody, this.cfg.lockCustodyLong, V2_SIDE_LONG),
        this.client.accounts.fetchMarket(this.cfg.targetCustody, this.cfg.lockCustodyShort, V2_SIDE_SHORT),
      ]);
      const longSize = bnToBigInt(longMkt.collectivePosition.sizeAmount);
      const shortSize = bnToBigInt(shortMkt.collectivePosition.sizeAmount);
      oiNet = longSize - shortSize;
      oiTotal = longSize + shortSize;
      // Use the long-side average entry as a coarse mark proxy.
      mark = bnToBigInt(longMkt.collectivePosition.averageEntryPrice.price);
    } catch {
      // Fail-soft: return zeros — the strategy will detect markPriceTicks=0
      // and skip quoting that iteration.
    }
    return {
      markPriceTicks: mark,
      vpinBps: 0, // V2 has no VPIN; widening branch collapses to zero.
      tickSize: 1n, // V2 prices are encoded in oracle exponent — we treat
                    // each unit of `price` as one tick.
      minBaseLots: 1n,
      oiImbalanceLots: oiNet,
      oiTotalLots: oiTotal,
      currentBatch: 0n, // No batch concept in V2.
    };
  }

  async fetchTrader(trader: PublicKey): Promise<TraderSnapshot | null> {
    let collateral = 0n;
    let openPositions = 0;
    try {
      const basket = await this.client.accounts.fetchBasket(trader);
      for (const p of basket.positions) {
        if (!p.isActive) continue;
        collateral += bnToBigInt(p.collateralAmount);
        openPositions += 1;
      }
    } catch {
      // Basket may not exist yet — treat as empty trader.
    }
    return {
      collateralQuoteLots: collateral,
      // V2 doesn't expose a per-session realized PnL field. The MM bot's
      // drawdown kill switch should layer an off-chain accumulator on top
      // (subscribe to fill events + integrate locally).
      realizedPnlQuoteLots: 0n,
      openPositions,
    };
  }

  async fetchPosition(_market: PublicKey, trader: PublicKey): Promise<PositionSnapshot | null> {
    try {
      const basket = await this.client.accounts.fetchBasket(trader);
      let signed = 0n;
      let entryRef = 0n;
      for (const p of basket.positions) {
        if (!p.isActive) continue;
        const isLong = 'long' in p.side;
        const size = bnToBigInt(p.sizeAmount);
        signed += isLong ? size : -size;
        if (entryRef === 0n) entryRef = bnToBigInt(p.entryPrice.price);
      }
      if (signed === 0n && entryRef === 0n) return null;
      return { signedSizeLots: signed, entryPriceTicks: entryRef };
    } catch {
      return null;
    }
  }

  async fetchOpenOrderSeqs(_market: PublicKey, trader: PublicKey): Promise<bigint[]> {
    try {
      const basket = await this.client.accounts.fetchBasket(trader);
      const seqs: bigint[] = [];
      for (const o of basket.orders) {
        if (!o.isActive) continue;
        // Pack side into bit 63 so cancel knows which side to address.
        // bit 63 = 1 → short, 0 → long.
        const sideBit = 'short' in o.side ? 1n << 63n : 0n;
        seqs.push(BigInt(o.orderId) | sideBit);
      }
      return seqs;
    } catch {
      return [];
    }
  }

  async buildQuoteInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    bidTicks: bigint;
    askTicks: bigint;
    sizeLots: bigint;
  }): Promise<TransactionInstruction[]> {
    const out: TransactionInstruction[] = [];
    const sizeBN = new BN(args.sizeLots.toString());
    const collateralBN = this.cfg.collateralPerQuote ?? sizeBN;
    const ZERO_PRICE: V2OraclePrice = { price: new BN(0), exponent: this.cfg.priceExponent };

    if (args.bidTicks > 0n) {
      const params: V2PlaceLimitOrderParams = {
        limitPrice: { price: new BN(args.bidTicks.toString()), exponent: this.cfg.priceExponent },
        collateralAmount: collateralBN,
        sizeAmount: sizeBN,
        stopLossPrice: ZERO_PRICE,
        takeProfitPrice: ZERO_PRICE,
      };
      const res = await this.client.placeLimitOrder(
        this.cfg.targetSymbol,
        this.cfg.collateralSymbol,
        V2_SIDE_LONG,
        this.cfg.poolConfig,
        params,
        this.cfg.receivingSymbol,
      );
      out.push(...res.instructions);
    }
    if (args.askTicks > 0n) {
      const params: V2PlaceLimitOrderParams = {
        limitPrice: { price: new BN(args.askTicks.toString()), exponent: this.cfg.priceExponent },
        collateralAmount: collateralBN,
        sizeAmount: sizeBN,
        stopLossPrice: ZERO_PRICE,
        takeProfitPrice: ZERO_PRICE,
      };
      const res = await this.client.placeLimitOrder(
        this.cfg.targetSymbol,
        this.cfg.collateralSymbol,
        V2_SIDE_SHORT,
        this.cfg.poolConfig,
        params,
        this.cfg.receivingSymbol,
      );
      out.push(...res.instructions);
    }
    return out;
  }

  async buildCancelInstructions(args: {
    trader: PublicKey;
    market: PublicKey;
    seqs: bigint[];
  }): Promise<TransactionInstruction[]> {
    // Per IDL: editLimitOrder with limitPrice=0 AND sizeAmount=0 cancels.
    const out: TransactionInstruction[] = [];
    const ZERO_PRICE: V2OraclePrice = { price: new BN(0), exponent: this.cfg.priceExponent };
    const SIDE_MASK = (1n << 63n) - 1n;
    for (const seq of args.seqs) {
      const isShort = (seq >> 63n) === 1n;
      const orderId = Number(seq & SIDE_MASK);
      const params: V2EditLimitOrderParams = {
        orderId,
        limitPrice: ZERO_PRICE,
        sizeAmount: new BN(0),
        stopLossPrice: ZERO_PRICE,
        takeProfitPrice: ZERO_PRICE,
      };
      const res = await this.client.editLimitOrder(
        this.cfg.targetSymbol,
        this.cfg.collateralSymbol,
        isShort ? V2_SIDE_SHORT : V2_SIDE_LONG,
        this.cfg.poolConfig,
        params,
        this.cfg.receivingSymbol,
      );
      out.push(...res.instructions);
    }
    return out;
  }

  async sendTx(instructions: TransactionInstruction[], signers: Keypair[]): Promise<string> {
    const tx = new Transaction().add(...instructions);
    tx.recentBlockhash = (await this.connection.getLatestBlockhash()).blockhash;
    tx.feePayer = signers[0]!.publicKey;
    tx.sign(...signers);
    const sig = await this.connection.sendRawTransaction(tx.serialize());
    await this.connection.confirmTransaction(sig, 'confirmed');
    return sig;
  }
}

// ─── Helpers ──────────────────────────────────────────────────────────

function bnToBigInt(bn: BN): bigint {
  return BigInt(bn.toString());
}
