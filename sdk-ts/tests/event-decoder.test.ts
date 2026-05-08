import { describe, expect, test } from 'bun:test';
import { decodeEventsFromLogs, decodeOne } from '../src/event-decoder.ts';

describe('Event decoder', () => {
  test('returns null for non-program-data line', () => {
    expect(decodeOne('Program log: hello')).toBeNull();
  });

  test('returns null for malformed program data', () => {
    expect(decodeOne('Program data: not-base64-====!!')).toBeNull();
  });

  test('returns null for unrelated base64 data', () => {
    // Base64 that doesn't match any of our event discriminators.
    expect(decodeOne('Program data: AAAAAAAAAAA=')).toBeNull();
  });

  test('decodeEventsFromLogs returns empty array for non-event logs', () => {
    const logs = [
      'Program FBookV1111111111111111111111111111111111111 invoke [1]',
      'Program log: hello',
      'Program FBookV1111111111111111111111111111111111111 success',
    ];
    expect(decodeEventsFromLogs(logs)).toEqual([]);
  });

  test('decodeEventsFromLogs filters non-data lines', () => {
    const logs = [
      'Program log: starting batch',
      'Program data: AAAAAAAAAAA=',
      'Program log: done',
    ];
    // None of these are real events → empty result.
    expect(decodeEventsFromLogs(logs)).toEqual([]);
  });
});
