// Pure quote computation — Avellaneda-Stoikov inventory skew + VPIN spread
// + OI-imbalance widening. No I/O. Lifted from the original single-market
// bot but reorganized into a stand-alone module for testability and reuse
// by the backtester.

import type { MarketSnapshot, QuoteParams } from './types.ts';

export interface QuoteOutput {
  bidTicks: bigint;
  askTicks: bigint;
  fairValueTicks: bigint;
  effectiveSpreadBps: number;
  empty: boolean;
}

export interface ComputeQuoteInput {
  market: MarketSnapshot;
  inventorySignedLots: bigint;
  capitalQuoteLots: bigint;
  params: QuoteParams;
  skipBid?: boolean;
  skipAsk?: boolean;
}

export function computeQuote(args: ComputeQuoteInput): QuoteOutput {
  const { market, inventorySignedLots, capitalQuoteLots, params } = args;
  const empty: QuoteOutput = {
    bidTicks: 0n,
    askTicks: 0n,
    fairValueTicks: market.markPriceTicks,
    effectiveSpreadBps: 0,
    empty: true,
  };
  if (market.markPriceTicks <= 0n || capitalQuoteLots <= 0n) return empty;

  // Inventory fraction — signed bps of capital. Positive = net long.
  const invMag = inventorySignedLots < 0n ? -inventorySignedLots : inventorySignedLots;
  const invNotional = invMag * market.markPriceTicks * market.tickSize;
  const sign = inventorySignedLots < 0n ? -1 : 1;
  const invFractionBps =
    capitalQuoteLots > 0n
      ? Number((invNotional * 10_000n) / capitalQuoteLots) * sign
      : 0;
  const inventorySkewBps = -params.inventorySkewBpsPerUnit * (invFractionBps / 10_000);

  // OI imbalance widens spread (Glosten-Milgrom).
  const oiImbalanceMag =
    market.oiTotalLots > 0n
      ? Math.abs(Number(market.oiImbalanceLots) / Number(market.oiTotalLots))
      : 0;

  const vpinFraction = market.vpinBps / 10_000;
  const halfSpreadBps =
    params.baseSpreadBps +
    params.vpinSpreadAlpha * vpinFraction * 10_000 +
    params.oiImbalanceSpreadCoef * oiImbalanceMag * 10_000;

  const mark = Number(market.markPriceTicks);
  const fair = mark * (1 + inventorySkewBps / 10_000);
  const fairTicks = BigInt(Math.round(fair));
  const halfSpreadFraction = halfSpreadBps / 10_000;
  const bidPx = fair * (1 - halfSpreadFraction);
  const askPx = fair * (1 + halfSpreadFraction);

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
