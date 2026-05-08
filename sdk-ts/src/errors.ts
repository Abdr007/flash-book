// Numbered error families from programs/flash-book/src/errors.rs.
// Use to classify on-chain errors returned by the Anchor program.

export enum FlashBookErrorCode {
  // 1000-1099 numerical
  ArithmeticOverflow = 1000,
  ArithmeticUnderflow = 1001,
  DivisionByZero = 1002,
  OutOfRange = 1003,
  NonFinite = 1004,

  // 1100-1199 account / authority
  Unauthorized = 1100,
  AlreadyInitialized = 1101,
  NotInitialized = 1102,
  WrongMarket = 1103,
  WrongTrader = 1104,
  StaleVersion = 1105,

  // 1200-1299 order intake
  SizeBelowMinLot = 1200,
  PriceNotOnTick = 1201,
  ZeroSize = 1202,
  ZeroPrice = 1203,
  InsufficientCollateral = 1204,
  NewPositionsPaused = 1205,
  LeverageExceeded = 1206,
  PostOnlyCross = 1207,
  RateLimited = 1208,
  PositionSizeCapExceeded = 1209,

  // 1300-1399 matcher
  BufferFull = 1300,
  SelfTrade = 1301,
  EmptyBatch = 1302,
  MarkOutsideBand = 1303,

  // 1400-1499 margin / liquidation
  TraderLiquidatable = 1400,
  TooManyScenarios = 1401,
  LiquidationStale = 1402,
  NotLiquidatable = 1403,

  // 1500-1599 insurance fund
  InsuranceBelowFloor = 1500,
  InsuranceExhausted = 1501,

  // 1600-1699 commit-reveal
  CommitMismatch = 1600,
  CommitExpired = 1601,
  CommitDuplicate = 1602,
  CommitBondRequired = 1603,

  // 1700-1799 delegation
  NotDelegated = 1700,
  AlreadyDelegated = 1701,
  DelegationExpired = 1702,
  ForceIncludeUnsupported = 1703,

  // 1800-1899 oracle
  OracleTooStale = 1800,
  OracleConfidenceTooWide = 1801,
  OraclePausedConfidence = 1802,
}

export function errorFamily(code: number): string {
  if (code >= 1000 && code < 1100) return 'numerical';
  if (code >= 1100 && code < 1200) return 'account/authority';
  if (code >= 1200 && code < 1300) return 'order_intake';
  if (code >= 1300 && code < 1400) return 'matcher';
  if (code >= 1400 && code < 1500) return 'margin/liquidation';
  if (code >= 1500 && code < 1600) return 'insurance';
  if (code >= 1600 && code < 1700) return 'commit_reveal';
  if (code >= 1700 && code < 1800) return 'delegation';
  if (code >= 1800 && code < 1900) return 'oracle';
  return 'unknown';
}

export function errorName(code: number): string | undefined {
  return FlashBookErrorCode[code as FlashBookErrorCode];
}
