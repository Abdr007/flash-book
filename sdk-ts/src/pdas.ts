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

/// Wave 21 program-split sister IDs. SKELETONS as of wave 21 phase 1
/// (deployable; no functional ixs beyond `ping`). See
/// `docs/V3_WAVE21_MODULAR.md` for the migration plan.
export const FLASH_BOOK_ORDERS_PROGRAM_ID = new PublicKey(
  '2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5',
);

export const FLASH_BOOK_FLP_PROGRAM_ID = new PublicKey(
  'eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M',
);

export const FLASH_BOOK_VAULTS_PROGRAM_ID = new PublicKey(
  'GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt',
);

export const TOKEN_PROGRAM_ID = new PublicKey(
  'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
);

export const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey(
  'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL',
);

/// SPL Associated Token Account program — used as the program account when
/// CPI-creating ATAs.
export const ATA_PROGRAM_ID = ASSOCIATED_TOKEN_PROGRAM_ID;

/// Derive the canonical Associated Token Account address for (owner, mint).
/// Equivalent to `getAssociatedTokenAddressSync` from `@solana/spl-token`,
/// inlined to avoid pulling that package as a runtime dependency.
export function associatedTokenAddress(
  owner: PublicKey,
  mint: PublicKey,
  tokenProgramId: PublicKey = TOKEN_PROGRAM_ID,
  associatedTokenProgramId: PublicKey = ASSOCIATED_TOKEN_PROGRAM_ID,
): PublicKey {
  const [address] = PublicKey.findProgramAddressSync(
    [owner.toBuffer(), tokenProgramId.toBuffer(), mint.toBuffer()],
    associatedTokenProgramId,
  );
  return address;
}

const MARKET_SEED = Buffer.from('market');
const COMMIT_BUFFER_SEED = Buffer.from('commit_buffer');
const INSURANCE_FUND_SEED = Buffer.from('insurance_fund');
const FLP_EXPOSURE_SEED = Buffer.from('flp_exposure');
const TRADER_STATE_SEED = Buffer.from('trader_state');
const POSITION_SEED = Buffer.from('position');
const LP_POSITION_SEED = Buffer.from('lp_position');
const TRIGGER_ORDER_SEED = Buffer.from('trigger');
const TWAP_ORDER_SEED = Buffer.from('twap');
const ICEBERG_ORDER_SEED = Buffer.from('iceberg');
const VAULT_SEED = Buffer.from('vault');
const VAULT_POSITION_SEED = Buffer.from('vault_position');
const MARKET_BOND_SEED = Buffer.from('market_bond');
const MARKET_BOOK_SEED = Buffer.from('market_book');

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

export function lpPositionPda(
  lp: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([LP_POSITION_SEED, lp.toBuffer()], programId);
}

export function triggerOrderPda(
  market: PublicKey,
  trader: PublicKey,
  triggerId: number,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive(
    [TRIGGER_ORDER_SEED, market.toBuffer(), trader.toBuffer(), Buffer.from([triggerId & 0xff])],
    programId,
  );
}

export function twapOrderPda(
  market: PublicKey,
  trader: PublicKey,
  twapId: number,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive(
    [TWAP_ORDER_SEED, market.toBuffer(), trader.toBuffer(), Buffer.from([twapId & 0xff])],
    programId,
  );
}

export function icebergOrderPda(
  market: PublicKey,
  trader: PublicKey,
  icebergId: number,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive(
    [ICEBERG_ORDER_SEED, market.toBuffer(), trader.toBuffer(), Buffer.from([icebergId & 0xff])],
    programId,
  );
}

export function vaultPda(
  strategist: PublicKey,
  vaultId: number,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive(
    [VAULT_SEED, strategist.toBuffer(), Buffer.from([vaultId & 0xff])],
    programId,
  );
}

export function vaultPositionPda(
  vault: PublicKey,
  depositor: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive(
    [VAULT_POSITION_SEED, vault.toBuffer(), depositor.toBuffer()],
    programId,
  );
}

export function marketBondPda(
  market: PublicKey,
  depositor: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([MARKET_BOND_SEED, market.toBuffer(), depositor.toBuffer()], programId);
}

/// Hypertree-backed v2 orderbook account. PDA seeds [b"market_book", market].
export function marketBookPda(
  market: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([MARKET_BOOK_SEED, market.toBuffer()], programId);
}

/// Wave 21 phase 3a: TriggerOrderAccountV3 PDA owned by `flash-book-orders`.
/// Distinct from core's `triggerOrderPda` (core seed `b"trigger"`,
/// orders seed `b"trigger_v3"`) so a trader can hold both legacy and
/// v3 triggers during the migration window.
const TRIGGER_V3_SEED = Buffer.from('trigger_v3');
export function triggerOrderV3Pda(
  market: PublicKey,
  trader: PublicKey,
  triggerId: number,
  ordersProgramId: PublicKey = FLASH_BOOK_ORDERS_PROGRAM_ID,
): DerivedPda {
  return derive(
    [
      TRIGGER_V3_SEED,
      market.toBuffer(),
      trader.toBuffer(),
      Buffer.from([triggerId & 0xff]),
    ],
    ordersProgramId,
  );
}

/// Wave 21 phase 3b: TwapOrderAccountV3 PDA under flash-book-orders.
const TWAP_V3_SEED = Buffer.from('twap_v3');
export function twapOrderV3Pda(
  market: PublicKey,
  trader: PublicKey,
  twapId: number,
  ordersProgramId: PublicKey = FLASH_BOOK_ORDERS_PROGRAM_ID,
): DerivedPda {
  return derive(
    [
      TWAP_V3_SEED,
      market.toBuffer(),
      trader.toBuffer(),
      Buffer.from([twapId & 0xff]),
    ],
    ordersProgramId,
  );
}

/// Wave 21 phase 3c: IcebergOrderAccountV3 PDA under flash-book-orders.
const ICEBERG_V3_SEED = Buffer.from('iceberg_v3');
export function icebergOrderV3Pda(
  market: PublicKey,
  trader: PublicKey,
  icebergId: number,
  ordersProgramId: PublicKey = FLASH_BOOK_ORDERS_PROGRAM_ID,
): DerivedPda {
  return derive(
    [
      ICEBERG_V3_SEED,
      market.toBuffer(),
      trader.toBuffer(),
      Buffer.from([icebergId & 0xff]),
    ],
    ordersProgramId,
  );
}

/// Wave 21 phase 8: per-market FLP exposure under flash-book-flp.
/// One per market — independently ER-delegatable.
const FLP_PER_MARKET_SEED = Buffer.from('flp_per_market');
export function flpExposurePerMarketV3Pda(
  market: PublicKey,
  flpProgramId: PublicKey = FLASH_BOOK_FLP_PROGRAM_ID,
): DerivedPda {
  return derive([FLP_PER_MARKET_SEED, market.toBuffer()], flpProgramId);
}

/// Wave 21 phase 9: vault account under flash-book-vaults.
const VAULT_V3_SEED = Buffer.from('vault_v3');
export function vaultV3Pda(
  strategist: PublicKey,
  vaultId: number,
  vaultsProgramId: PublicKey = FLASH_BOOK_VAULTS_PROGRAM_ID,
): DerivedPda {
  return derive(
    [VAULT_V3_SEED, strategist.toBuffer(), Buffer.from([vaultId & 0xff])],
    vaultsProgramId,
  );
}

/// Wave 21 phase 9: vault depositor share account under flash-book-vaults.
const VAULT_POSITION_V3_SEED = Buffer.from('vault_position_v3');
export function vaultPositionV3Pda(
  vault: PublicKey,
  depositor: PublicKey,
  vaultsProgramId: PublicKey = FLASH_BOOK_VAULTS_PROGRAM_ID,
): DerivedPda {
  return derive(
    [VAULT_POSITION_V3_SEED, vault.toBuffer(), depositor.toBuffer()],
    vaultsProgramId,
  );
}

/// Wave 21 phase 2: per-wrapper-program CPI signer PDA. Each wrapper
/// (orders / flp / vaults) signs core CPI calls with this PDA. Core
/// validates the signer matches one of the 3 expected derivations.
const CPI_AUTHORITY_SEED = Buffer.from('cpi_authority');
export function wrapperCpiAuthorityPda(
  wrapperProgramId: PublicKey,
): DerivedPda {
  return derive([CPI_AUTHORITY_SEED], wrapperProgramId);
}

/// Per-market leverage-tier table — wave 20a (HL pattern).
/// PDA seeds [b"leverage_tiers", market]. OPTIONAL — markets without
/// this account fall back to the 2-tier (baseline + concentration_extra)
/// model already on `MarketAccount.params`.
const LEVERAGE_TIERS_SEED = Buffer.from('leverage_tiers');
export function marketLeverageTiersPda(
  market: PublicKey,
  programId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([LEVERAGE_TIERS_SEED, market.toBuffer()], programId);
}

// ─── MagicBlock ER delegation PDAs ────────────────────────────────────
//
// Mirrors `programs/flash-book/src/er.rs`:
//   - Delegation program: DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh
//   - Delegate buffer    : [b"buffer",              delegated_account]
//                          OWNED BY THE OWNER PROGRAM (= our program ID)
//   - Delegation record  : [b"delegation",          delegated_account]
//                          OWNED BY THE DELEGATION PROGRAM
//   - Metadata           : [b"delegation-metadata", delegated_account]
//                          OWNED BY THE DELEGATION PROGRAM
//
// SDK callers use these to compose `delegateMarketBookIx` /
// `delegateMarketIx` accounts WITHOUT pulling the magicblock SDK as a
// runtime dependency.

export const MAGICBLOCK_DELEGATION_PROGRAM_ID = new PublicKey(
  'DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh',
);

const DELEGATE_BUFFER_TAG = Buffer.from('buffer');
const DELEGATION_RECORD_TAG = Buffer.from('delegation');
const DELEGATION_METADATA_TAG = Buffer.from('delegation-metadata');

/// Delegate buffer PDA — lives under THIS program (the account owner),
/// not under the delegation program. Pass `programId` for non-default
/// owner programs.
export function delegateBufferPda(
  delegatedAccount: PublicKey,
  ownerProgramId: PublicKey = FLASH_BOOK_PROGRAM_ID,
): DerivedPda {
  return derive([DELEGATE_BUFFER_TAG, delegatedAccount.toBuffer()], ownerProgramId);
}

/// Delegation record PDA — lives under the MagicBlock delegation program.
export function delegationRecordPda(delegatedAccount: PublicKey): DerivedPda {
  return derive(
    [DELEGATION_RECORD_TAG, delegatedAccount.toBuffer()],
    MAGICBLOCK_DELEGATION_PROGRAM_ID,
  );
}

/// Delegation metadata PDA — lives under the MagicBlock delegation program.
export function delegationMetadataPda(delegatedAccount: PublicKey): DerivedPda {
  return derive(
    [DELEGATION_METADATA_TAG, delegatedAccount.toBuffer()],
    MAGICBLOCK_DELEGATION_PROGRAM_ID,
  );
}
