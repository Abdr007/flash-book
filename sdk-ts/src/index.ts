// Public API of the Flash Book TypeScript SDK.

export {
  FLASH_BOOK_PROGRAM_ID,
  marketPda,
  orderBufferPda,
  commitBufferPda,
  insuranceFundPda,
  flpExposurePda,
  traderStatePda,
  positionPda,
  type DerivedPda,
} from './pdas.ts';

export {
  defaultMajorMarketParams,
  defaultInsuranceFundParams,
  type MarketParamsRaw,
  type InsuranceFundInitParams,
} from './params.ts';

export {
  FlashBookClient,
  IDL,
} from './client.ts';

export {
  FlashBookErrorCode,
  errorFamily,
  errorName,
} from './errors.ts';

export type {
  MarketInitializedEvent,
  BatchClearedEvent,
  CollateralDepositedEvent,
  CollateralWithdrawnEvent,
  FillAppliedEvent,
  FlpFillAppliedEvent,
  LiquidationInjectedEvent,
  MarketStatusChangedEvent,
  MarketParamsUpdatedEvent,
  MarketAuthorityTransferredEvent,
  FlashBookEvent,
} from './events.ts';
export { MarketStatus } from './events.ts';
