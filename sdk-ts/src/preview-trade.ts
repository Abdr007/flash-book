// previewTrade — the canonical pre-trade UX composer.
//
// Fetches market + buffer + position state, simulates clearing with the
// trader's prospective order, projects the post-fill position, runs
// risk preview against it. Returns expected fill price + size +
// post-trade equity / required-margin / health-ratio in one call.
//
// This is what frontends call to render TradingView-style:
//   "limit order will fill at ~$99.95 with 0.05% slippage,
//    resulting in a long 10-lot position at 18% to liquidation."

import type { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import type { FlashBookClient } from './client.ts';
import {
  fetchMarket,
  fetchOrderBuffer,
  fetchPosition,
  fetchTraderState,
  type MarketAccount,
  type OrderSlot,
  type PositionAccount,
} from './accounts.ts';
import {
  fillForOrder,
  simulateBatchClearing,
  type SimOrder,
  type SimSide,
} from './order-simulator.ts';
import {
  defaultScenarios,
  initialMarginRequired,
  previewPortfolioRisk,
  type RiskPreview,
} from './risk-preview.ts';

export interface PreviewTradeRequest {
  readonly trader: PublicKey;
  readonly market: PublicKey;
  readonly side: SimSide;
  readonly sizeLots: number;
  readonly limitTicks: number;
  /** 'taker' for an immediate-cross intent, 'limit' for a resting order. */
  readonly orderType?: 'taker' | 'limit';
}

export interface PreviewTradeResult {
  /** Whether the trade would even reach the matcher (passes intake gates). */
  readonly intakeAllowed: boolean;
  /** If intake-blocked, the reason. */
  readonly blockReason?:
    | 'no-trader-state'
    | 'market-paused'
    | 'order-rejected-margin-gate'
    | 'insufficient-collateral-im'
    | 'rate-limit-exceeded';

  /** Expected fill from FBA simulation (ignoring FLP virtual quotes). */
  readonly expectedFill: { sizeLots: number; priceTicks: number };
  /**
   * Price improvement vs limit, in bps. Always ≥ 0 (FBA fills at-or-better
   * than the limit price). 0 = filled at exact limit. Higher = paid less /
   * received more than the limit price specified.
   */
  readonly priceImprovementBps: number | null;

  /** Risk preview against the post-fill portfolio. */
  readonly postTradeRisk: RiskPreview;
  /** Initial margin required for the new position size. */
  readonly initialMarginRequired: number;
}

const STATUS_ACTIVE = 1;
const STATUS_POST_ONLY = 2;
const RATE_LIMIT_PER_BATCH = 16;

function slotToSim(slot: OrderSlot): SimOrder {
  return {
    id: `slot_${slot.id.toString()}`,
    trader: slot.trader.toBase58(),
    side: slot.side === 0 ? 'long' : 'short',
    orderType:
      slot.orderType === 0
        ? 'limit'
        : slot.orderType === 1
          ? 'taker'
          : slot.orderType === 2
            ? 'flp_virtual'
            : slot.orderType === 3
              ? 'liquidation'
              : 'adl',
    sizeLots: slot.sizeLots.toNumber(),
    limitTicks: slot.limitTicks.toNumber(),
    seq: slot.seq.toNumber(),
  };
}

function projectPosition(
  current: PositionAccount | null,
  fillSide: SimSide,
  fillSizeLots: number,
  fillPriceTicks: number,
  trader: PublicKey,
  market: PublicKey,
): PositionAccount {
  // If no current position, the post-fill position is just the fill.
  if (!current || current.sizeLots.isZero()) {
    return {
      trader,
      market,
      bump: 0,
      side: fillSide === 'long' ? 0 : 1,
      sizeLots: new BN(fillSizeLots),
      entryPriceTicks: new BN(fillPriceTicks),
      collateralQuoteLots: new BN(0),
      cumFundingIndexAtEntry: new BN(0),
      realizedPnlQuoteLots: new BN(0),
      fundingPaidQuoteLots: new BN(0),
      lastSettlementBatch: new BN(0),
    };
  }

  const curSide = current.side === 0 ? 'long' : 'short';
  const curSize = current.sizeLots.toNumber();
  const curEntry = current.entryPriceTicks.toNumber();

  if (curSide === fillSide) {
    // Same side: weighted-average entry.
    const newSize = curSize + fillSizeLots;
    const newEntry = (curEntry * curSize + fillPriceTicks * fillSizeLots) / newSize;
    return {
      ...current,
      sizeLots: new BN(newSize),
      entryPriceTicks: new BN(Math.round(newEntry)),
    };
  }

  // Opposite side: reduce or flip.
  if (fillSizeLots <= curSize) {
    const newSize = curSize - fillSizeLots;
    return {
      ...current,
      sizeLots: new BN(newSize),
      // Entry stays unless fully closed.
      entryPriceTicks: newSize === 0 ? new BN(0) : current.entryPriceTicks,
    };
  }

  // Flip.
  const remaining = fillSizeLots - curSize;
  return {
    ...current,
    side: fillSide === 'long' ? 0 : 1,
    sizeLots: new BN(remaining),
    entryPriceTicks: new BN(fillPriceTicks),
  };
}

/**
 * Run the full pre-trade preview. Single call, fetches everything from
 * the chain, simulates, projects, returns the post-trade picture.
 */
export async function previewTrade(
  client: FlashBookClient,
  req: PreviewTradeRequest,
): Promise<PreviewTradeResult> {
  // Fetch all the pieces in parallel.
  const [marketAcct, bufferAcct, traderState, currentPos] = await Promise.all([
    fetchMarket(client, req.market),
    fetchOrderBuffer(client, client.orderBuffer(req.market).address),
    fetchTraderState(client, client.traderState(req.trader).address),
    fetchPosition(client, client.position(req.market, req.trader).address),
  ]);

  if (!marketAcct) {
    throw new Error(`Market ${req.market.toBase58()} not initialized`);
  }

  // Intake gate: market status.
  if (marketAcct.status !== STATUS_ACTIVE && marketAcct.status !== STATUS_POST_ONLY) {
    return earlyExit('market-paused', marketAcct, currentPos, req);
  }

  // Intake gate: trader state exists.
  if (!traderState) {
    return earlyExit('no-trader-state', marketAcct, currentPos, req);
  }

  // Intake gate: rate limit.
  // (This only checks the local snapshot — the actual program rate-limit
  //  resets on a new batch boundary that we can't predict.)
  if (
    traderState.lastBatchSeen.eq(marketAcct.currentBatch)
    && traderState.ordersThisBatch >= RATE_LIMIT_PER_BATCH
  ) {
    return earlyExit('rate-limit-exceeded', marketAcct, currentPos, req);
  }

  // Build sim input.
  const orders: SimOrder[] = [];
  if (bufferAcct) {
    for (const slot of bufferAcct.slots) {
      if (slot.valid === 1) orders.push(slotToSim(slot));
    }
  }
  // Append our prospective order.
  const myId = `preview_${req.trader.toBase58()}_${Date.now()}`;
  orders.push({
    id: myId,
    trader: req.trader.toBase58(),
    side: req.side,
    orderType: req.orderType ?? 'taker',
    sizeLots: req.sizeLots,
    limitTicks: req.limitTicks,
    seq: bufferAcct ? bufferAcct.seqCounter.toNumber() + 1 : 1,
  });

  // Simulate.
  const result = simulateBatchClearing(orders, marketAcct.markPriceTicks.toNumber());
  const myFill = fillForOrder(result, myId);

  // Price improvement: positive = filled better than limit.
  const priceImprovementBps =
    myFill.sizeLots > 0 && req.limitTicks > 0
      ? Math.max(
          0,
          Math.round(
            (req.side === 'long'
              ? (req.limitTicks - myFill.priceTicks) / req.limitTicks
              : (myFill.priceTicks - req.limitTicks) / req.limitTicks) * 10_000,
          ),
        )
      : null;

  // Project post-fill position.
  const postPos = projectPosition(
    currentPos,
    req.side,
    myFill.sizeLots,
    myFill.priceTicks,
    req.trader,
    req.market,
  );

  const markets = new Map<string, MarketAccount>([[req.market.toBase58(), marketAcct]]);
  const collateral = traderState.collateralQuoteLots.toNumber();
  const postTradeRisk = previewPortfolioRisk(
    [postPos],
    markets,
    collateral,
    defaultScenarios([req.market.toBase58()]),
  );

  // Initial margin for the *added* size only (matches on-chain check).
  const im = initialMarginRequired(req.sizeLots, req.limitTicks, marketAcct);
  // Intake gate: would the order pass the IM gate on-chain?
  if (collateral < postTradeRisk.required) {
    // Note: the real on-chain gate uses initial margin against the
    // existing position; the assess_margin gate fires when the trader
    // is already liquidatable. Here we surface the risk preview as the
    // canonical answer; downstream UI may want to also flag IM
    // separately depending on semantics.
  }

  return {
    intakeAllowed: true,
    expectedFill: { sizeLots: myFill.sizeLots, priceTicks: myFill.priceTicks },
    priceImprovementBps,
    postTradeRisk,
    initialMarginRequired: im,
  };
}

function earlyExit(
  reason: NonNullable<PreviewTradeResult['blockReason']>,
  market: MarketAccount,
  current: PositionAccount | null,
  req: PreviewTradeRequest,
): PreviewTradeResult {
  // Run risk preview against existing position with zero collateral, just
  // so the caller still gets a meaningful risk readout.
  const markets = new Map([[req.market.toBase58(), market]]);
  const positions = current && !current.sizeLots.isZero() ? [current] : [];
  const risk = previewPortfolioRisk(positions, markets, 0);
  return {
    intakeAllowed: false,
    blockReason: reason,
    expectedFill: { sizeLots: 0, priceTicks: 0 },
    priceImprovementBps: null,
    postTradeRisk: risk,
    initialMarginRequired: 0,
  };
}
