import { describe, expect, test } from 'bun:test';
import {
  FlashBookErrorCode,
  errorFamily,
  errorName,
} from '../src/errors.ts';

describe('Error code classification', () => {
  test('numerical family', () => {
    expect(errorFamily(FlashBookErrorCode.ArithmeticOverflow)).toBe('numerical');
    expect(errorFamily(FlashBookErrorCode.DivisionByZero)).toBe('numerical');
  });

  test('order_intake family', () => {
    expect(errorFamily(FlashBookErrorCode.SizeBelowMinLot)).toBe('order_intake');
    expect(errorFamily(FlashBookErrorCode.RateLimited)).toBe('order_intake');
  });

  test('matcher family', () => {
    expect(errorFamily(FlashBookErrorCode.SelfTrade)).toBe('matcher');
    expect(errorFamily(FlashBookErrorCode.BufferFull)).toBe('matcher');
  });

  test('liquidation family', () => {
    expect(errorFamily(FlashBookErrorCode.NotLiquidatable)).toBe('margin/liquidation');
  });

  test('insurance family', () => {
    expect(errorFamily(FlashBookErrorCode.InsuranceExhausted)).toBe('insurance');
  });

  test('commit-reveal family', () => {
    expect(errorFamily(FlashBookErrorCode.CommitMismatch)).toBe('commit_reveal');
    expect(errorFamily(FlashBookErrorCode.CommitExpired)).toBe('commit_reveal');
  });

  test('unknown code returns "unknown"', () => {
    expect(errorFamily(9999)).toBe('unknown');
    expect(errorFamily(0)).toBe('unknown');
  });

  test('errorName returns enum name', () => {
    expect(errorName(FlashBookErrorCode.ArithmeticOverflow)).toBe('ArithmeticOverflow');
    expect(errorName(FlashBookErrorCode.SelfTrade)).toBe('SelfTrade');
    expect(errorName(FlashBookErrorCode.NotLiquidatable)).toBe('NotLiquidatable');
  });

  test('errorName for unknown code is undefined', () => {
    expect(errorName(9999)).toBeUndefined();
  });
});
