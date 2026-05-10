// Real-time event subscription via `logsSubscribe`.
//
// Wraps the canonical RPC subscription with the typed event decoder so
// callers receive `FlashBookEvent` directly — no log parsing.

import type { Connection, Logs, PublicKey } from '@solana/web3.js';
import { decodeEventsFromLogs } from './event-decoder.ts';
import type { EventStreamCallback, EventSubscription } from './event-decoder.ts';
import { FLASH_BOOK_PROGRAM_ID } from './pdas.ts';

/// Mirror of the on-chain `encode_order_id` (state_v2.rs) — inlined here
/// to avoid a circular import with `index.ts` (which re-exports the
/// public copy of this function).
function encodeOrderIdLocal(
  priceTicks: bigint,
  seq: bigint,
  sideIsBid: boolean,
): bigint {
  const PRICE_MASK = (1n << 48n) - 1n;
  const SEQ_MASK = (1n << 16n) - 1n;
  const U64_MASK = (1n << 64n) - 1n;
  const price = priceTicks & PRICE_MASK;
  const seqLow = seq & SEQ_MASK;
  const raw = (price << 16n) | seqLow;
  return sideIsBid ? raw ^ U64_MASK : raw;
}

/**
 * Subscribe to all Flash Book events on this connection. Returns an
 * `EventSubscription` whose `.unsubscribe()` cancels the subscription.
 *
 * `commitment` defaults to 'confirmed' which gives ~400ms latency on
 * mainnet without sacrificing finality reliability.
 */
export function subscribeToProgramEvents(
  connection: Connection,
  callback: EventStreamCallback,
  options: {
    programId?: PublicKey;
    commitment?: 'processed' | 'confirmed' | 'finalized';
  } = {},
): EventSubscription {
  const programId = options.programId ?? FLASH_BOOK_PROGRAM_ID;
  const commitment = options.commitment ?? 'confirmed';

  const subId = connection.onLogs(
    programId,
    (logs: Logs, ctx: { slot: number }) => {
      if (logs.err !== null) return; // skip failed transactions
      const events = decodeEventsFromLogs(logs.logs);
      for (const ev of events) {
        callback(ev, ctx.slot, logs.signature);
      }
    },
    commitment,
  );

  return {
    unsubscribe: () => {
      void connection.removeOnLogsListener(subId);
    },
  };
}

/// One open order tracked by the trader-orders subscription. Mirrors
/// `OpenOrder` from `bot/src/market-maker.ts` so the bot can use this
/// helper without re-mapping the shape.
export interface TraderOpenOrder {
  orderId: bigint;
  side: 'long' | 'short';
  priceTicks: bigint;
  seq: bigint;
}

/// Per-trader event handlers. Each fires when an OrderPlacedV2Event /
/// OrderCancelledV2Event matching the (market, trader) filter lands
/// in the program log stream.
export interface TraderOrderCallbacks {
  onPlaced?: (order: TraderOpenOrder, slot: number, signature: string) => void;
  onCancelled?: (orderSeq: bigint, side: 'long' | 'short', slot: number, signature: string) => void;
  /// Filter by trader pubkey base58. If omitted, fires for every trader
  /// in the market (useful for global indexers; not for MM session state).
  traderFilterBase58?: string;
  /// Filter by market pubkey base58. If omitted, fires for every market.
  marketFilterBase58?: string;
}

/// Subscribe to OrderPlacedV2Event + OrderCancelledV2Event filtered by
/// (market, trader). Returns the same EventSubscription shape as
/// `subscribeToProgramEvents` so it can be unsubscribed cleanly on
/// shutdown.
///
/// Production-grade order tracking pattern (HL/dYdX): the bot's local
/// open-orders map is hydrated from this subscription, NOT from polling
/// the on-chain account. This means:
///
///   • Stateless restart — start a fresh subscription, optionally backfill
///     via `getSignaturesForAddress` + `getTransaction` to replay missed
///     events from a known slot.
///   • No race between place + read — the subscription is the source of
///     truth; local-tracking-on-place is just an immediate-feedback
///     optimistic write that the subscription confirms.
///   • Cancel + fill events update the same map.
export function subscribeToTraderOrders(
  connection: Connection,
  callbacks: TraderOrderCallbacks,
  options: {
    programId?: PublicKey;
    commitment?: 'processed' | 'confirmed' | 'finalized';
  } = {},
): EventSubscription {
  return subscribeToProgramEvents(
    connection,
    (ev, slot, signature) => {
      switch (ev.name) {
        case 'OrderPlacedV2Event': {
          const d = ev.data;
          if (callbacks.traderFilterBase58
            && d.trader.toBase58() !== callbacks.traderFilterBase58) return;
          if (callbacks.marketFilterBase58
            && d.market.toBase58() !== callbacks.marketFilterBase58) return;
          const sideIsBid = d.side === 0;
          const priceTicks = BigInt(d.priceTicks.toString());
          const seq = BigInt(d.seq.toString());
          callbacks.onPlaced?.({
            orderId: encodeOrderIdLocal(priceTicks, seq, sideIsBid),
            side: sideIsBid ? 'long' : 'short',
            priceTicks,
            seq,
          }, slot, signature);
          break;
        }
        case 'OrderCancelledV2Event': {
          const d = ev.data;
          if (callbacks.traderFilterBase58
            && d.trader.toBase58() !== callbacks.traderFilterBase58) return;
          if (callbacks.marketFilterBase58
            && d.market.toBase58() !== callbacks.marketFilterBase58) return;
          const sideIsBid = d.side === 0;
          callbacks.onCancelled?.(
            BigInt(d.orderSeq.toString()),
            sideIsBid ? 'long' : 'short',
            slot,
            signature,
          );
          break;
        }
        default:
          break;
      }
    },
    options,
  );
}
