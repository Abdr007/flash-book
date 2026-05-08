// Shared test fixtures.

import { DEFAULT_MAJOR_MARKET_PARAMS } from '../src/index.ts';
import type { MarketParams, MarketState, Position, Side } from '../src/types.ts';

export const TEST_PARAMS: MarketParams = DEFAULT_MAJOR_MARKET_PARAMS;

export function makeTestMarket(symbol: string, oraclePrice: number): MarketState {
  return {
    symbol,
    oraclePrice,
    oracleConfidence: 0,
    markPrice: oraclePrice,
    cumFundingIndex: 0,
    lastFundingRate: 0,
    vpin: 0,
    openInterestLong: 0,
    openInterestShort: 0,
    recentClearingPrices: [],
    totalFeesCollected: 0,
    totalToxicityTaxCollected: 0,
    totalLiquidationsCount: 0,
    params: TEST_PARAMS,
    bidBook: new Map(),
    askBook: new Map(),
  };
}

export function makeTestPosition(
  market: string,
  side: Side,
  size: number,
  entryPrice: number,
  trader = 'T',
): Position {
  return {
    trader,
    market,
    side,
    size,
    entryPrice,
    collateral: 0,
    cumFundingIndexAtEntry: 0,
    realizedPnl: 0,
    fundingPaid: 0,
  };
}
