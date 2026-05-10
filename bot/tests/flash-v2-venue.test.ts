import { describe, expect, test, mock } from 'bun:test';
import { Keypair, PublicKey, type TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';
import {
  FlashV2Venue,
  V2_SIDE_LONG,
  V2_SIDE_SHORT,
  type FlashV2VenueConfig,
  type MagicTradeClient,
  type V2BasketAccount,
  type V2MarketAccount,
} from '../src/flash-v2-venue.ts';

const targetCustody = new PublicKey('11111111111111111111111111111112');
const lockLong = new PublicKey('11111111111111111111111111111113');
const lockShort = new PublicKey('11111111111111111111111111111114');

const baseConfig: FlashV2VenueConfig = {
  poolConfig: {} as never,
  targetSymbol: 'SOL',
  collateralSymbol: 'USDC',
  targetCustody,
  lockCustodyLong: lockLong,
  lockCustodyShort: lockShort,
  priceExponent: -8,
};

function dummyIx(): TransactionInstruction {
  // A minimal placeholder instruction — we never send it; the test only
  // asserts how many of these the venue produces.
  return {
    programId: new PublicKey('11111111111111111111111111111111'),
    keys: [],
    data: Buffer.alloc(0),
  };
}

function mockMarket(longSize: number, shortSize: number, mark = 100_000): V2MarketAccount {
  return {
    collectivePosition: {
      sizeAmount: new BN(longSize),
      sizeUsd: new BN(longSize * mark),
      averageEntryPrice: { price: new BN(mark), exponent: -8 },
    },
  };
}

function makeMockClient(opts: {
  basket?: V2BasketAccount;
  longMarket?: V2MarketAccount;
  shortMarket?: V2MarketAccount;
  placeReturns?: TransactionInstruction[];
  editReturns?: TransactionInstruction[];
}): MagicTradeClient & {
  placeCalls: { side: 'long' | 'short'; params: { sizeAmount: BN; limitPrice: { price: BN } } }[];
  editCalls: { orderId: number; sizeAmount: BN; limitPrice: { price: BN } }[];
} {
  const placeCalls: { side: 'long' | 'short'; params: { sizeAmount: BN; limitPrice: { price: BN } } }[] = [];
  const editCalls: { orderId: number; sizeAmount: BN; limitPrice: { price: BN } }[] = [];
  return {
    placeCalls,
    editCalls,
    placeLimitOrder: mock(async (_t, _c, side, _p, params) => {
      placeCalls.push({
        side: 'long' in side ? 'long' : 'short',
        params: { sizeAmount: params.sizeAmount, limitPrice: { price: params.limitPrice.price } },
      });
      return { instructions: opts.placeReturns ?? [dummyIx()], additionalSigners: [] };
    }),
    editLimitOrder: mock(async (_t, _c, _side, _p, params) => {
      editCalls.push({
        orderId: params.orderId,
        sizeAmount: params.sizeAmount,
        limitPrice: { price: params.limitPrice.price },
      });
      return { instructions: opts.editReturns ?? [dummyIx()], additionalSigners: [] };
    }),
    accounts: {
      fetchMarket: mock(async (_target, lock, side) => {
        if ('long' in side) return opts.longMarket ?? mockMarket(0, 0);
        return opts.shortMarket ?? mockMarket(0, 0);
      }),
      fetchBasket: mock(async () => {
        if (!opts.basket) throw new Error('no basket');
        return opts.basket;
      }),
    },
  };
}

const dummyConn = { getLatestBlockhash: async () => ({ blockhash: 'x' }) } as never;

describe('FlashV2Venue', () => {
  test('name is stable', () => {
    const v = new FlashV2Venue(makeMockClient({}), dummyConn, baseConfig);
    expect(v.name).toBe('flash-v2');
  });

  test('fetchMarket aggregates collective OI from both side markets', async () => {
    const client = makeMockClient({
      longMarket: mockMarket(500, 0, 100_500),
      shortMarket: mockMarket(300, 0, 99_500),
    });
    const v = new FlashV2Venue(client, dummyConn, baseConfig);
    const snap = await v.fetchMarket(new PublicKey('11111111111111111111111111111111'));
    expect(snap).not.toBeNull();
    expect(snap!.markPriceTicks).toBe(100_500n);
    expect(snap!.oiImbalanceLots).toBe(200n); // 500 long - 300 short
    expect(snap!.oiTotalLots).toBe(800n);
    expect(snap!.vpinBps).toBe(0);
  });

  test('fetchMarket fail-soft returns zeros on SDK error', async () => {
    const client = makeMockClient({});
    client.accounts.fetchMarket = mock(async () => {
      throw new Error('rpc fail');
    });
    const v = new FlashV2Venue(client, dummyConn, baseConfig);
    const snap = await v.fetchMarket(new PublicKey('11111111111111111111111111111111'));
    expect(snap!.markPriceTicks).toBe(0n);
    expect(snap!.oiImbalanceLots).toBe(0n);
  });

  test('fetchTrader sums collateral across active positions', async () => {
    const trader = Keypair.generate().publicKey;
    const basket: V2BasketAccount = {
      owner: trader,
      positionsActive: true,
      ordersActive: false,
      positions: [
        {
          market: new PublicKey('11111111111111111111111111111115'),
          side: V2_SIDE_LONG,
          sizeAmount: new BN(10),
          collateralAmount: new BN(1_000),
          entryPrice: { price: new BN(100_000), exponent: -8 },
          isActive: true,
        },
        {
          market: new PublicKey('11111111111111111111111111111116'),
          side: V2_SIDE_SHORT,
          sizeAmount: new BN(5),
          collateralAmount: new BN(500),
          entryPrice: { price: new BN(101_000), exponent: -8 },
          isActive: true,
        },
        {
          // Inactive — should be excluded from the sum.
          market: new PublicKey('11111111111111111111111111111117'),
          side: V2_SIDE_LONG,
          sizeAmount: new BN(7),
          collateralAmount: new BN(999),
          entryPrice: { price: new BN(0), exponent: -8 },
          isActive: false,
        },
      ],
      orders: [],
    };
    const v = new FlashV2Venue(makeMockClient({ basket }), dummyConn, baseConfig);
    const t = await v.fetchTrader(trader);
    expect(t!.collateralQuoteLots).toBe(1_500n);
    expect(t!.openPositions).toBe(2);
  });

  test('fetchPosition computes signed size as long - short', async () => {
    const trader = Keypair.generate().publicKey;
    const basket: V2BasketAccount = {
      owner: trader,
      positionsActive: true,
      ordersActive: false,
      positions: [
        {
          market: new PublicKey('11111111111111111111111111111115'),
          side: V2_SIDE_LONG,
          sizeAmount: new BN(10),
          collateralAmount: new BN(1_000),
          entryPrice: { price: new BN(100_000), exponent: -8 },
          isActive: true,
        },
        {
          market: new PublicKey('11111111111111111111111111111116'),
          side: V2_SIDE_SHORT,
          sizeAmount: new BN(3),
          collateralAmount: new BN(300),
          entryPrice: { price: new BN(101_000), exponent: -8 },
          isActive: true,
        },
      ],
      orders: [],
    };
    const v = new FlashV2Venue(makeMockClient({ basket }), dummyConn, baseConfig);
    const p = await v.fetchPosition(targetCustody, trader);
    expect(p!.signedSizeLots).toBe(7n); // 10 - 3
  });

  test('fetchOpenOrderSeqs packs side into bit 63', async () => {
    const trader = Keypair.generate().publicKey;
    const basket: V2BasketAccount = {
      owner: trader,
      positionsActive: false,
      ordersActive: true,
      positions: [],
      orders: [
        {
          market: new PublicKey('11111111111111111111111111111115'),
          orderId: 5,
          side: V2_SIDE_LONG,
          limitPrice: { price: new BN(99_950), exponent: -8 },
          sizeAmount: new BN(1),
          isActive: true,
        },
        {
          market: new PublicKey('11111111111111111111111111111115'),
          orderId: 7,
          side: V2_SIDE_SHORT,
          limitPrice: { price: new BN(100_050), exponent: -8 },
          sizeAmount: new BN(1),
          isActive: true,
        },
      ],
    };
    const v = new FlashV2Venue(makeMockClient({ basket }), dummyConn, baseConfig);
    const orders = await v.fetchOpenOrders(targetCustody, trader);
    expect(orders.length).toBe(2);
    // Long order: orderId = 5, side = 'long'.
    const longOrder = orders.find((o) => o.side === 'long');
    expect(longOrder?.orderId).toBe(5n);
    // Short order: orderId = 7, side = 'short'.
    const shortOrder = orders.find((o) => o.side === 'short');
    expect(shortOrder?.orderId).toBe(7n);
  });

  test('buildQuoteInstructions calls placeLimitOrder for each non-zero side', async () => {
    const client = makeMockClient({});
    const v = new FlashV2Venue(client, dummyConn, baseConfig);
    const trader = Keypair.generate().publicKey;
    const ixs = await v.buildQuoteInstructions({
      trader,
      market: targetCustody,
      bidTicks: 99_950n,
      askTicks: 100_050n,
      sizeLots: 1n,
    });
    expect(ixs.length).toBe(2);
    expect(client.placeCalls.length).toBe(2);
    expect(client.placeCalls.find((c) => c.side === 'long')!.params.limitPrice.price.toString()).toBe('99950');
    expect(client.placeCalls.find((c) => c.side === 'short')!.params.limitPrice.price.toString()).toBe('100050');
  });

  test('buildQuoteInstructions skips a side when its tick = 0', async () => {
    const client = makeMockClient({});
    const v = new FlashV2Venue(client, dummyConn, baseConfig);
    const ixs = await v.buildQuoteInstructions({
      trader: Keypair.generate().publicKey,
      market: targetCustody,
      bidTicks: 0n,
      askTicks: 100_050n,
      sizeLots: 1n,
    });
    expect(ixs.length).toBe(1);
    expect(client.placeCalls.length).toBe(1);
    expect(client.placeCalls[0]!.side).toBe('short');
  });

  test('buildCancelInstructions issues editLimitOrder with size=0 + price=0', async () => {
    const client = makeMockClient({});
    const v = new FlashV2Venue(client, dummyConn, baseConfig);
    const ixs = await v.buildCancelInstructions({
      trader: Keypair.generate().publicKey,
      market: targetCustody,
      orders: [
        { orderId: 3n, side: 'long', priceTicks: 0n, seq: 3n },
        { orderId: 9n, side: 'short', priceTicks: 0n, seq: 9n },
      ],
    });
    expect(ixs.length).toBe(2);
    expect(client.editCalls.length).toBe(2);
    expect(client.editCalls.every((c) => c.sizeAmount.isZero() && c.limitPrice.price.isZero())).toBe(true);
    expect(client.editCalls.find((c) => c.orderId === 3)).toBeDefined();
    expect(client.editCalls.find((c) => c.orderId === 9)).toBeDefined();
  });
});
