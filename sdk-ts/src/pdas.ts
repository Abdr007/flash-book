// PDA derivation helpers for the Flash Book program.
//
// Seed conventions (must match programs/flash-book/src/state.rs and lib.rs):
//
//   market           ["market", base_mint, quote_mint]
//   order_buffer     ["order_buffer", market]
//   commit_buffer    ["commit_buffer", market]
//   insurance_fund   ["insurance_fund"]
//   flp_exposure     ["flp_exposure"]
//   trader_state     ["trader_state", trader]
//   position         ["position", market, trader]

import { PublicKey } from '@solana/web3.js';

export const FLASH_BOOK_PROGRAM_ID = new PublicKey(
  'FBookV1111111111111111111111111111111111111',
);

const MARKET_SEED = Buffer.from('market');
const ORDER_BUFFER_SEED = Buffer.from('order_buffer');
const COMMIT_BUFFER_SEED = Buffer.from('commit_buffer');
const INSURANCE_FUND_SEED = Buffer.from('insurance_fund');
const FLP_EXPOSURE_SEED = Buffer.from('flp_exposure');
const TRADER_STATE_SEED = Buffer.from('trader_state');
const POSITION_SEED = Buffer.from('position');

export interface DerivedPda {
  readonly address: PublicKey;
  readonly bump: number;
}

function derive(seeds: Array<Buffer | Uint8Array>, programId: PublicKey): DerivedPda {
  const [address, bump] = PublicKey.findProgramAddressSync(seeds, programId);
  return { address, bump };
}

export function marketPda(
  baseMint: PublicKey,
  quoteMint: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([MARKET_SEED, baseMint.toBuffer(), quoteMint.toBuffer()], programId);
}

export function orderBufferPda(
  market: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([ORDER_BUFFER_SEED, market.toBuffer()], programId);
}

export function commitBufferPda(
  market: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([COMMIT_BUFFER_SEED, market.toBuffer()], programId);
}

export function insuranceFundPda(programId: PublicKey = FLASH_BOOK_PROGRAM_ID): DerivedPda {
  return derive([INSURANCE_FUND_SEED], programId);
}

export function flpExposurePda(programId: PublicKey = FLASH_BOOK_PROGRAM_ID): DerivedPda {
  return derive([FLP_EXPOSURE_SEED], programId);
}

export function traderStatePda(
  trader: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([TRADER_STATE_SEED, trader.toBuffer()], programId);
}

export function positionPda(
  market: PublicKey,
  trader: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([POSITION_SEED, market.toBuffer(), trader.toBuffer()], programId);
}
