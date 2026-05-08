// FlashBookEngine — top-level orchestrator.
//
// Per batch:
//   1. Advance funding index for every market.
//   2. For each market:
//      a. Gather order buffer (limit orders, revealed taker orders).
//      b. Detect liquidation candidates from prior-batch mark, inject liq orders.
//      c. Generate FLP virtual quotes.
//      d. Run FBA Walrasian clearing.
//      e. Apply fills: position updates, fees, FLP state, OI, VPIN.
//      f. Update mark price (TWAP + oracle band).
//   3. Resolve bankruptcies via insurance fund + ADL waterfall.
//   4. Sweep expired commit-reveal entries.
//   5. Verify invariants.

import { advanceFundingIndex, settleFunding } from './funding.ts';
import { generateFlpQuotes } from './flp-quoter.ts';
import {
  computeShortfall,
  detectLiquidations,
  generateLiquidationOrders,
  makeLiquidationEvent,
} from './liquidation.ts';
import { clearBatch } from './matcher.ts';
import {
  contributeFromFees,
  contributeFromLiqPenalty,
  contributeFromToxicityTax,
  coverShortfall,
  createInsuranceFund,
  newPositionsAllowed,
} from './insurance.ts';
import { generateScenarios, initialMarginRequired } from './risk.ts';
import {
  CommitRevealRegistry,
  type RevealPayload,
} from './commit-reveal.ts';
import { VpinCalculator } from './vpin.ts';
import { oracleBand, pushAndTwap, safeNumber } from './math.ts';
import {
  type AdlEvent,
  ADL_COUNTERPARTY_ID,
  type BatchResult,
  type EngineConfig,
  type Fill,
  FLP_TRADER_ID,
  type FlpState,
  type InsuranceFund,
  type LiquidationEvent,
  type MarketBatchResult,
  type MarketParams,
  type MarketState,
  type Order,
  type Position,
  type Side,
  type StressScenario,
} from './types.ts';

export interface AddMarketArgs {
  readonly symbol: string;
  readonly initialOraclePrice: number;
  readonly initialFlpCapital: number;
  readonly params: MarketParams;
}

export interface SubmitLimitArgs {
  readonly trader: string;
  readonly market: string;
  readonly side: Side;
  readonly size: number;
  readonly limitPrice: number;
  readonly postOnly?: boolean;
}

export interface SubmitTakerArgs {
  readonly trader: string;
  readonly market: string;
  readonly side: Side;
  readonly size: number;
  readonly limitPrice: number;
}

export class FlashBookEngine {
  private readonly markets = new Map<string, MarketState>();
  private readonly positionsByTrader = new Map<string, Position[]>();
  private readonly collateralByTrader = new Map<string, number>();
  private readonly orderBufferByMarket = new Map<string, Order[]>();
  private readonly vpinByMarket = new Map<string, VpinCalculator>();
  private readonly flpState: FlpState = {
    totalCapital: 0,
    netPositionByMarket: new Map(),
    realizedPnl: 0,
  };
  private readonly insuranceFund: InsuranceFund;
  private readonly commitRegistry = new CommitRevealRegistry();
  private readonly scenarios: StressScenario[];
  private orderIdSeq = 0;
  private currentBatch = 0;
  private lastBatchTimeMs: number | null = null;

  constructor(private readonly config: EngineConfig) {
    this.scenarios = [...config.scenarios];
    this.insuranceFund = createInsuranceFund(config.insuranceFund);
  }

  // ─── Setup ──────────────────────────────────────────────────────

  addMarket(args: AddMarketArgs): void {
    if (this.markets.has(args.symbol)) {
      throw new Error(`Market already exists: ${args.symbol}`);
    }
    const market: MarketState = {
      symbol: args.symbol,
      oraclePrice: args.initialOraclePrice,
      oracleConfidence: 0,
      markPrice: args.initialOraclePrice,
      cumFundingIndex: 0,
      lastFundingRate: 0,
      vpin: 0,
      openInterestLong: 0,
      openInterestShort: 0,
      recentClearingPrices: [],
      totalFeesCollected: 0,
      totalToxicityTaxCollected: 0,
      totalLiquidationsCount: 0,
      params: args.params,
      bidBook: new Map(),
      askBook: new Map(),
    };
    this.markets.set(args.symbol, market);
    this.orderBufferByMarket.set(args.symbol, []);
    this.vpinByMarket.set(
      args.symbol,
      new VpinCalculator(args.params.vpinBucketSize, args.params.vpinEmaWindow),
    );
    this.flpState.totalCapital += args.initialFlpCapital;

    // Re-derive scenarios any time markets change.
    if (this.scenarios.length === 0 || !this.config.scenarios.length) {
      this.scenarios.length = 0;
      const generated = generateScenarios([...this.markets.keys()]);
      for (const s of generated) this.scenarios.push(s);
    }
  }

  updateOraclePrice(symbol: string, price: number, confidence = 0): void {
    const m = this.markets.get(symbol);
    if (!m) throw new Error(`Unknown market: ${symbol}`);
    if (!Number.isFinite(price) || price <= 0) {
      throw new Error(`Invalid oracle price for ${symbol}: ${price}`);
    }
    m.oraclePrice = price;
    m.oracleConfidence = confidence;
  }

  // ─── Account management ─────────────────────────────────────────

  deposit(trader: string, amount: number): void {
    if (!Number.isFinite(amount) || amount <= 0) {
      throw new Error(`Invalid deposit amount: ${amount}`);
    }
    const cur = this.collateralByTrader.get(trader) ?? 0;
    this.collateralByTrader.set(trader, cur + amount);
  }

  withdraw(trader: string, amount: number): boolean {
    const cur = this.collateralByTrader.get(trader) ?? 0;
    if (amount <= 0 || amount > cur) return false;

    // Cannot withdraw if positions exist and would go below initial margin.
    const positions = this.positionsByTrader.get(trader);
    if (positions && positions.length > 0) {
      const remaining = cur - amount;
      let imRequired = 0;
      for (const p of positions) {
        const m = this.markets.get(p.market);
        if (!m) continue;
        imRequired += initialMarginRequired(p.side, p.size, m.markPrice, m);
      }
      if (remaining < imRequired) return false;
    }

    this.collateralByTrader.set(trader, cur - amount);
    return true;
  }

  collateral(trader: string): number {
    return this.collateralByTrader.get(trader) ?? 0;
  }

  positionsOf(trader: string): ReadonlyArray<Position> {
    return this.positionsByTrader.get(trader) ?? [];
  }

  // ─── Order intake ───────────────────────────────────────────────

  submitLimitOrder(args: SubmitLimitArgs): Order {
    const m = this.requireMarket(args.market);
    this.guardOrderInputs(args.size, args.limitPrice);
    const order: Order = {
      id: this.nextOrderId('limit'),
      market: args.market,
      trader: args.trader,
      side: args.side,
      size: args.size,
      limitPrice: args.limitPrice,
      type: 'limit',
      timestamp: Date.now(),
      postOnly: args.postOnly === true,
    };
    this.orderBufferByMarket.get(m.symbol)!.push(order);
    return order;
  }

  /**
   * Direct taker submission (use for tests or non-MEV-resistant flows).
   * Production traders should use commit/reveal instead.
   */
  submitTakerOrder(args: SubmitTakerArgs): Order {
    const m = this.requireMarket(args.market);
    this.guardOrderInputs(args.size, args.limitPrice);
    if (!newPositionsAllowed(this.insuranceFund)) {
      throw new Error('New positions paused (insurance fund below threshold)');
    }
    // Check initial margin upfront.
    const collateral = this.collateral(args.trader);
    const im = initialMarginRequired(args.side, args.size, args.limitPrice, m);
    const positions = this.positionsByTrader.get(args.trader) ?? [];
    let imExisting = 0;
    for (const p of positions) {
      const pm = this.markets.get(p.market);
      if (!pm) continue;
      imExisting += initialMarginRequired(p.side, p.size, pm.markPrice, pm);
    }
    if (collateral < imExisting + im) {
      throw new Error(`Insufficient collateral for initial margin (${collateral} < ${imExisting + im})`);
    }
    const order: Order = {
      id: this.nextOrderId('taker'),
      market: args.market,
      trader: args.trader,
      side: args.side,
      size: args.size,
      limitPrice: args.limitPrice,
      type: 'taker',
      timestamp: Date.now(),
      postOnly: false,
    };
    this.orderBufferByMarket.get(m.symbol)!.push(order);
    return order;
  }

  submitCommit(args: { trader: string; market: string; hash: string; bondLamports: number }): void {
    if (!this.config.commitRevealEnabled) {
      throw new Error('commit-reveal disabled in config');
    }
    this.requireMarket(args.market);
    this.commitRegistry.registerCommit({
      hash: args.hash,
      trader: args.trader,
      market: args.market,
      bondLamports: args.bondLamports,
      currentBatch: this.currentBatch,
      expireInBatches: this.config.commitExpiryBatches,
    });
  }

  submitReveal(payload: RevealPayload): boolean {
    if (!this.config.commitRevealEnabled) {
      throw new Error('commit-reveal disabled in config');
    }
    this.requireMarket(payload.market);
    const order = this.commitRegistry.redeem({
      payload,
      currentBatch: this.currentBatch,
      nowMs: Date.now(),
      orderIdSeq: ++this.orderIdSeq,
    });
    if (!order) return false;
    this.orderBufferByMarket.get(payload.market)!.push(order);
    return true;
  }

  // ─── Batch execution ────────────────────────────────────────────

  runBatch(nowMs: number): BatchResult {
    const blockDelta = this.lastBatchTimeMs == null ? 0 : Math.max(0, nowMs - this.lastBatchTimeMs);
    this.lastBatchTimeMs = nowMs;
    this.currentBatch += 1;
    const batchNum = this.currentBatch;

    // Step 1: advance funding indices on every market.
    for (const market of this.markets.values()) {
      advanceFundingIndex(market, blockDelta);
    }

    // Step 1b: refresh OI counters from authoritative position state.
    this.recomputeOpenInterest();

    // Step 2: detect liquidation candidates BEFORE clearing (uses prior mark).
    const candidates = detectLiquidations(
      this.positionsByTrader,
      this.collateralByTrader,
      this.markets,
      this.scenarios,
    );
    const liquidationOrders = generateLiquidationOrders({
      candidates,
      markets: this.markets,
      nowMs,
      batchNum,
    });

    // Step 3: per-market batch clear.
    const perMarket = new Map<string, MarketBatchResult>();
    const liquidationEvents: LiquidationEvent[] = [];
    const adlEvents: AdlEvent[] = [];
    let insuranceDelta = 0;

    for (const market of this.markets.values()) {
      const buffer = this.orderBufferByMarket.get(market.symbol) ?? [];

      // FLP virtual quotes for this batch.
      const flpQuotes = generateFlpQuotes({
        market,
        poolCapitalUsd: this.flpState.totalCapital,
        poolNetUsd: this.flpNetUsd(market.symbol),
        poolGrossUtilization: this.flpGrossUtilization(),
        nowMs,
        batchNum,
      });

      // Pull liquidation orders for this market.
      const liqForMarket = liquidationOrders.filter((o) => o.market === market.symbol);

      const batchOrders: Order[] = [...buffer, ...flpQuotes.orders, ...liqForMarket];

      const result = clearBatch({
        market: market.symbol,
        batchNum,
        nowMs,
        orders: batchOrders,
        priorMarkPrice: market.markPrice,
        params: market.params,
        vpin: market.vpin,
      });

      // Apply fills.
      let flpQuotesUsed = 0;
      for (const fill of result.fills) {
        this.applyFill(market, fill);
        if (fill.makerTrader === FLP_TRADER_ID || fill.takerTrader === FLP_TRADER_ID) {
          flpQuotesUsed += 1;
        }
      }

      // Update mark price as TWAP of recent clearing prices, banded by oracle.
      let newMark = market.markPrice;
      if (result.clearingVolume > 0) {
        const twap = pushAndTwap(market.recentClearingPrices, result.clearingPrice, market.params.twapWindow);
        newMark = oracleBand(twap, market.oraclePrice, market.params.oracleBandBps);
      } else {
        // No fills — let mark drift toward oracle within band.
        newMark = oracleBand(market.markPrice, market.oraclePrice, market.params.oracleBandBps);
      }
      market.markPrice = newMark;

      // Process liquidation bankruptcies for this market's filled liquidation orders.
      const liqFillsByLiqOrder = new Map<string, Fill>();
      for (const fill of result.fills) {
        if (fill.takerId.startsWith('liq_')) liqFillsByLiqOrder.set(fill.takerId, fill);
      }
      for (const cand of candidates) {
        for (const pos of cand.positions) {
          if (pos.market !== market.symbol) continue;
          const liqOrderId = `liq_${cand.trader}_${pos.market}_b${batchNum}`;
          const fill = liqFillsByLiqOrder.get(liqOrderId);
          if (!fill) continue;
          const sf = computeShortfall(pos, fill.price, market);
          let coveredFromInsurance = 0;
          let bankruptShortfall = 0;
          if (sf.shortfall > 0) {
            const res = coverShortfall(this.insuranceFund, sf.shortfall);
            coveredFromInsurance = res.covered;
            bankruptShortfall = res.remaining;
            insuranceDelta -= coveredFromInsurance;
            if (bankruptShortfall > 0) {
              const adl = this.runAdl(market, pos, bankruptShortfall, batchNum);
              for (const e of adl) adlEvents.push(e);
            }
          }
          const penaltyContribution = contributeFromLiqPenalty(this.insuranceFund, sf.liquidationPenalty);
          insuranceDelta += penaltyContribution;
          liquidationEvents.push(
            makeLiquidationEvent({
              position: pos,
              fillPrice: fill.price,
              market,
              insuranceFundContribution: coveredFromInsurance,
              bankruptShortfall,
              collateralRecovered: sf.collateralRecovered,
              batchNum,
            }),
          );
          market.totalLiquidationsCount += 1;
        }
      }

      // Drain processed orders from buffer (everything was offered to matcher;
      // limit-order remainders that didn't fill rest until we add a real book — for
      // the simulator, unfilled limits expire at end-of-batch).
      this.orderBufferByMarket.set(market.symbol, []);

      perMarket.set(market.symbol, {
        market: market.symbol,
        clearingPrice: result.clearingPrice,
        clearingVolume: result.clearingVolume,
        fills: result.fills,
        markPriceAfter: market.markPrice,
        fundingRateAfter: market.lastFundingRate,
        vpinAfter: market.vpin,
        flpQuotesUsed,
        flpQuotesGenerated: flpQuotes.orders.length,
      });
    }

    // Step 4: sweep expired commits.
    this.commitRegistry.sweepExpired(batchNum);

    // Step 5: verify invariants.
    const invariantsHeld = this.checkInvariants();

    return {
      batchNum,
      nowMs,
      perMarket,
      liquidations: liquidationEvents,
      adl: adlEvents,
      insuranceFundDelta: insuranceDelta,
      invariantsHeld,
    };
  }

  // ─── Fills + position updates ───────────────────────────────────

  private applyFill(market: MarketState, fill: Fill): void {
    // Update VPIN with the taker side.
    this.vpinByMarket.get(market.symbol)?.recordFill(fill.takerSide, fill.size);
    market.vpin = this.vpinByMarket.get(market.symbol)?.value ?? market.vpin;
    market.totalFeesCollected += fill.takerFee;
    market.totalToxicityTaxCollected += fill.toxicityTax;

    // Insurance fund contributions.
    contributeFromFees(this.insuranceFund, fill.takerFee);
    contributeFromToxicityTax(this.insuranceFund, fill.toxicityTax);

    const makerSide: Side = fill.takerSide === 'long' ? 'short' : 'long';

    // Update positions/state for both sides.
    this.applyToParticipant(market, fill.takerTrader, fill.takerSide, fill.size, fill.price);
    this.applyToParticipant(market, fill.makerTrader, makerSide, fill.size, fill.price);

    // OI is recomputed at end of batch from authoritative position state —
    // see recomputeOpenInterest(). Avoiding incremental updates here prevents
    // accounting drift when positions flip / partially close inside one fill.

    // Charge fees / pay rebates against trader collateral.
    if (fill.takerTrader !== FLP_TRADER_ID && fill.takerTrader !== ADL_COUNTERPARTY_ID) {
      this.adjustCollateral(fill.takerTrader, -(fill.takerFee + fill.toxicityTax));
    }
    if (fill.makerTrader !== FLP_TRADER_ID && fill.makerTrader !== ADL_COUNTERPARTY_ID) {
      this.adjustCollateral(fill.makerTrader, fill.makerRebate);
    }
  }

  private applyToParticipant(
    market: MarketState,
    trader: string,
    side: Side,
    size: number,
    price: number,
  ): void {
    if (trader === FLP_TRADER_ID) {
      this.applyFillToFlp(market, side, size, price);
      return;
    }
    if (trader === ADL_COUNTERPARTY_ID) {
      // ADL is a synthetic counterparty for sizing purposes; no position update.
      return;
    }
    this.applyFillToTrader(market, trader, side, size, price);
  }

  private applyFillToTrader(
    market: MarketState,
    trader: string,
    side: Side,
    size: number,
    price: number,
  ): void {
    const positions = this.positionsByTrader.get(trader) ?? [];
    const idx = positions.findIndex((p) => p.market === market.symbol);

    // Settle any pending funding before mutating.
    if (idx >= 0) {
      const existing = positions[idx] as Position;
      const owed = settleFunding(existing, market);
      this.adjustCollateral(trader, -owed);
    }

    if (idx === -1) {
      positions.push({
        trader,
        market: market.symbol,
        side,
        size,
        entryPrice: price,
        collateral: 0,
        cumFundingIndexAtEntry: market.cumFundingIndex,
        realizedPnl: 0,
        fundingPaid: 0,
      });
      this.positionsByTrader.set(trader, positions);
      return;
    }

    const pos = positions[idx] as Position;

    if (pos.side === side) {
      const newSize = pos.size + size;
      pos.entryPrice = (pos.entryPrice * pos.size + price * size) / newSize;
      pos.size = newSize;
    } else if (size <= pos.size) {
      const sign = pos.side === 'long' ? 1 : -1;
      const pnl = sign * size * (price - pos.entryPrice);
      pos.realizedPnl += pnl;
      this.adjustCollateral(trader, pnl);
      pos.size -= size;
      if (pos.size <= 1e-12) positions.splice(idx, 1);
    } else {
      const sign = pos.side === 'long' ? 1 : -1;
      const pnl = sign * pos.size * (price - pos.entryPrice);
      pos.realizedPnl += pnl;
      this.adjustCollateral(trader, pnl);
      const remaining = size - pos.size;
      pos.side = side;
      pos.size = remaining;
      pos.entryPrice = price;
      pos.cumFundingIndexAtEntry = market.cumFundingIndex;
    }
  }

  private applyFillToFlp(market: MarketState, side: Side, size: number, price: number): void {
    const cur = this.flpState.netPositionByMarket.get(market.symbol);
    if (!cur) {
      this.flpState.netPositionByMarket.set(market.symbol, { side, size, entryPrice: price });
      return;
    }
    if (cur.side === side) {
      const newSize = cur.size + size;
      cur.entryPrice = (cur.entryPrice * cur.size + price * size) / newSize;
      cur.size = newSize;
    } else if (size <= cur.size) {
      const sign = cur.side === 'long' ? 1 : -1;
      const pnl = sign * size * (price - cur.entryPrice);
      this.flpState.realizedPnl += pnl;
      this.flpState.totalCapital += pnl;
      cur.size -= size;
      if (cur.size <= 1e-12) {
        this.flpState.netPositionByMarket.delete(market.symbol);
      }
    } else {
      const sign = cur.side === 'long' ? 1 : -1;
      const pnl = sign * cur.size * (price - cur.entryPrice);
      this.flpState.realizedPnl += pnl;
      this.flpState.totalCapital += pnl;
      const remaining = size - cur.size;
      this.flpState.netPositionByMarket.set(market.symbol, {
        side,
        size: remaining,
        entryPrice: price,
      });
    }
  }

  private flpNetUsd(market: string): number {
    const pos = this.flpState.netPositionByMarket.get(market);
    if (!pos) return 0;
    const m = this.markets.get(market);
    if (!m) return 0;
    return (pos.side === 'long' ? 1 : -1) * pos.size * m.markPrice;
  }

  private flpGrossUtilization(): number {
    if (this.flpState.totalCapital <= 0) return 0;
    let gross = 0;
    for (const [symbol, pos] of this.flpState.netPositionByMarket) {
      const m = this.markets.get(symbol);
      if (!m) continue;
      gross += pos.size * m.markPrice;
    }
    return gross / this.flpState.totalCapital;
  }

  // ─── ADL ─────────────────────────────────────────────────────────

  private runAdl(market: MarketState, bankruptPos: Position, shortfall: number, batchNum: number): AdlEvent[] {
    // Find counter-positions on the SAME market with side opposite to bankruptPos.
    // E.g. if a long got bankrupt-liquidated, ADL the longest profitable shorts.
    const counterSide: Side = bankruptPos.side === 'long' ? 'long' : 'short';
    const candidates: Array<{ pos: Position; score: number }> = [];

    for (const positions of this.positionsByTrader.values()) {
      for (const pos of positions) {
        if (pos.market !== market.symbol || pos.side !== counterSide) continue;
        const sign = pos.side === 'long' ? 1 : -1;
        const unrealized = sign * pos.size * (market.markPrice - pos.entryPrice);
        if (unrealized <= 0) continue;
        const collateral = this.collateral(pos.trader);
        const leverage = (pos.size * market.markPrice) / Math.max(collateral, 1);
        const profitRatio = unrealized / Math.max(collateral, 1);
        candidates.push({ pos, score: profitRatio * leverage });
      }
    }

    candidates.sort((a, b) => b.score - a.score);

    const events: AdlEvent[] = [];
    let remaining = shortfall;
    for (const c of candidates) {
      if (remaining <= 0) break;
      const positionNotional = c.pos.size * market.markPrice;
      const notionalToAdl = Math.min(positionNotional, remaining);
      const sizeToAdl = notionalToAdl / market.markPrice;

      // Realize PnL on the ADL'd portion at current mark.
      const sign = c.pos.side === 'long' ? 1 : -1;
      const pnl = sign * sizeToAdl * (market.markPrice - c.pos.entryPrice);
      c.pos.realizedPnl += pnl;
      this.adjustCollateral(c.pos.trader, pnl);
      c.pos.size -= sizeToAdl;
      remaining -= notionalToAdl;

      events.push({
        trader: c.pos.trader,
        market: market.symbol,
        side: c.pos.side,
        size: sizeToAdl,
        price: market.markPrice,
        forcedExitReason: 'insurance_exhausted',
        batchNum,
      });

      if (c.pos.size <= 1e-12) {
        const arr = this.positionsByTrader.get(c.pos.trader);
        if (arr) {
          const i = arr.indexOf(c.pos);
          if (i >= 0) arr.splice(i, 1);
        }
      }
    }

    return events;
  }

  // ─── Helpers ─────────────────────────────────────────────────────

  private adjustCollateral(trader: string, delta: number): void {
    const cur = this.collateralByTrader.get(trader) ?? 0;
    this.collateralByTrader.set(trader, cur + delta);
  }

  /** Recompute OI counters from current trader positions. FLP positions are
   *  tracked separately and excluded from OI imbalance. */
  private recomputeOpenInterest(): void {
    for (const m of this.markets.values()) {
      m.openInterestLong = 0;
      m.openInterestShort = 0;
    }
    for (const positions of this.positionsByTrader.values()) {
      for (const pos of positions) {
        const m = this.markets.get(pos.market);
        if (!m) continue;
        if (pos.side === 'long') m.openInterestLong += pos.size;
        else m.openInterestShort += pos.size;
      }
    }
  }

  private guardOrderInputs(size: number, limitPrice: number): void {
    if (!Number.isFinite(size) || size <= 0) {
      throw new Error(`Invalid order size: ${size}`);
    }
    if (!Number.isFinite(limitPrice) || limitPrice <= 0) {
      throw new Error(`Invalid limit price: ${limitPrice}`);
    }
  }

  private requireMarket(symbol: string): MarketState {
    const m = this.markets.get(symbol);
    if (!m) throw new Error(`Unknown market: ${symbol}`);
    return m;
  }

  private nextOrderId(prefix: string): string {
    return `${prefix}_${++this.orderIdSeq}_b${this.currentBatch}`;
  }

  /**
   * Solvency invariant: sum(collateral) + flp_capital + insurance ==
   * sum(unrealized_pnl) + sum(realized_proceeds) + initial_endowment.
   *
   * For simplicity we check a weaker form: no negative collateral except
   * in transient liquidation states; total system value is finite.
   */
  private checkInvariants(): boolean {
    for (const [trader, c] of this.collateralByTrader) {
      if (!Number.isFinite(c)) return false;
      // Allow slight negative from rounding only.
      if (c < -1e-6) {
        const positions = this.positionsByTrader.get(trader);
        if (!positions || positions.length === 0) return false;
      }
    }
    if (!Number.isFinite(this.flpState.totalCapital)) return false;
    if (!Number.isFinite(this.insuranceFund.balance)) return false;
    if (this.insuranceFund.balance < 0) return false;
    return true;
  }

  // ─── Read-only views ─────────────────────────────────────────────

  marketState(symbol: string): MarketState {
    return this.requireMarket(symbol);
  }

  insuranceFundView(): Readonly<InsuranceFund> {
    return this.insuranceFund;
  }

  flpStateView(): Readonly<FlpState> {
    return this.flpState;
  }

  flpNetUsdAcrossMarkets(): number {
    let total = 0;
    for (const symbol of this.flpState.netPositionByMarket.keys()) {
      total += safeNumber(this.flpNetUsd(symbol));
    }
    return total;
  }

  currentBatchNumber(): number {
    return this.currentBatch;
  }

  pendingCommitsCount(): number {
    return this.commitRegistry.pendingCount();
  }
}
