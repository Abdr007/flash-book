// Real-time event subscription via `logsSubscribe`.
//
// Wraps the canonical RPC subscription with the typed event decoder so
// callers receive `FlashBookEvent` directly — no log parsing.

import type { Connection, Logs, PublicKey } from '@solana/web3.js';
import { decodeEventsFromLogs } from './event-decoder.ts';
import type { EventStreamCallback, EventSubscription } from './event-decoder.ts';
import { FLASH_BOOK_PROGRAM_ID } from './pdas.ts';

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
