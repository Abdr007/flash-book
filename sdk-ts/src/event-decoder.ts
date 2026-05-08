// Event log decoder.
//
// Anchor encodes events as `Program data: <base64>` log lines. The
// `BorshEventCoder` matches each line against the IDL's event variants
// and yields typed event objects. This is the canonical path for
// indexers and off-chain monitors.

import { BorshEventCoder } from '@coral-xyz/anchor';
import type { Event } from '@coral-xyz/anchor';
import { IDL } from './client.ts';
import type { FlashBookEvent } from './events.ts';

const coder = new BorshEventCoder(IDL);

/**
 * Decode all Flash Book events from a transaction's `logMessages`.
 * Returns events in the order they were emitted.
 */
export function decodeEventsFromLogs(logs: ReadonlyArray<string>): FlashBookEvent[] {
  const out: FlashBookEvent[] = [];
  for (const line of logs) {
    const event = decodeOne(line);
    if (event) out.push(event);
  }
  return out;
}

/**
 * Try to decode a single log line. Returns `null` if the line is not a
 * Flash Book program data line, or if it doesn't match any known event.
 */
export function decodeOne(line: string): FlashBookEvent | null {
  if (!line.startsWith('Program data: ')) return null;
  const data = line.slice('Program data: '.length);
  const decoded = coder.decode(data);
  if (!decoded) return null;
  return toFlashBookEvent(decoded);
}

function toFlashBookEvent(ev: Event): FlashBookEvent {
  // Anchor's Event has `{ name, data }` — `name` matches the Rust struct
  // name (PascalCase). We just pass through with our discriminated union.
  return { name: ev.name as FlashBookEvent['name'], data: ev.data } as FlashBookEvent;
}

/** Subscribe to `logsSubscribe` for the program and emit decoded events. */
export interface EventSubscription {
  unsubscribe: () => void;
}

export interface EventStreamCallback {
  (event: FlashBookEvent, slot: number, signature: string): void;
}
