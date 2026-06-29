//! Canonical PDA seed prefixes for the Pinocchio port.
//!
//! A PDA address = `find_program_address([PREFIX, ...components], program_id)`.
//! The scheme mirrors the anchor program's so the two derive parallel address
//! spaces (under their respective program ids). The bump is appended by the
//! runtime on derivation and must be re-supplied (as the final seed) when the
//! program signs CPIs as the PDA.

/// `[b"trader_state", trader]` — the per-trader collateral account.
pub const TRADER_STATE_SEED: &[u8] = b"trader_state";

/// `[b"insurance_fund"]` — the protocol singleton insurance fund.
pub const INSURANCE_SEED: &[u8] = b"insurance_fund";

/// `[b"market", base_mint, quote_mint]` — a market account.
pub const MARKET_SEED: &[u8] = b"market";

/// `[b"flp_exposure"]` — the protocol singleton FLP (pool-as-maker) exposure.
pub const FLP_EXPOSURE_SEED: &[u8] = b"flp_exposure";

/// `[b"lp_position", lp]` — a liquidity provider's FLP share position.
pub const LP_POSITION_SEED: &[u8] = b"lp_position";

/// `[b"leverage_tiers", market]` — a market's notional-banded MMR tier ladder.
pub const LEVERAGE_TIERS_SEED: &[u8] = b"leverage_tiers";

/// `[b"fee_tiers"]` — the protocol-singleton volume-based fee-tier table.
pub const FEE_TIERS_SEED: &[u8] = b"fee_tiers";

/// `[b"trigger_v3", market, trader, trigger_id]` — a v3 conditional (trigger)
/// order. Distinct prefix from any legacy trigger so the two never collide.
pub const TRIGGER_ORDER_SEED: &[u8] = b"trigger_v3";

/// `[b"twap_v3", market, trader, twap_id]` — a v3 TWAP (time-sliced) order.
pub const TWAP_ORDER_SEED: &[u8] = b"twap_v3";

/// `[b"iceberg_v3", market, trader, iceberg_id]` — a v3 iceberg order. Only the
/// displayed chunk rests on the book at a time; a keeper replenishes the next.
pub const ICEBERG_ORDER_SEED: &[u8] = b"iceberg_v3";

/// `[b"flp_per_market", market]` — a market-scoped FLP-v3 exposure account.
pub const FLP_PER_MARKET_SEED: &[u8] = b"flp_per_market";

/// `[b"flp_position_v3", exposure, lp]` — an LP's per-market FLP-v3 share position.
pub const FLP_POSITION_V3_SEED: &[u8] = b"flp_position_v3";

/// `[b"envelope", market]` — a market's envelope (price-band) config account.
pub const ENVELOPE_CONFIG_SEED: &[u8] = b"envelope";

/// `[b"oracle_config", market]` — a market's oracle config account.
pub const ORACLE_CONFIG_SEED: &[u8] = b"oracle_config";

/// `[b"side_accrual", market]` — a market's side-accrual (ADL) state account.
pub const SIDE_ACCRUAL_SEED: &[u8] = b"side_accrual";

/// `[b"vault_v3", strategist, vault_id]` — a v3 strategist vault account.
pub const VAULT_SEED: &[u8] = b"vault_v3";

/// `[b"vault_position_v3", vault, depositor]` — a depositor's share record in a
/// v3 vault.
pub const VAULT_POSITION_SEED: &[u8] = b"vault_position_v3";

/// `[b"haircut", market]` — a market's haircut (positive-PnL warmup) state.
pub const HAIRCUT_SEED: &[u8] = b"haircut";

/// `[b"position_haircut", market, position]` — a position's haircut state.
pub const POSITION_HAIRCUT_SEED: &[u8] = b"position_haircut";
pub const POSITION_LIQ_STATE_SEED: &[u8] = b"position_liq";
pub const JIT_LIQ_OFFER_SEED: &[u8] = b"jit_liq_offer";

/// `[b"session", owner, session_signer]` — a delegated session-signing token.
pub const SESSION_SEED: &[u8] = b"session";

/// `[b"er_margin", trader_state]` — a trader's ER margin attestation.
pub const ER_MARGIN_SEED: &[u8] = b"er_margin";
