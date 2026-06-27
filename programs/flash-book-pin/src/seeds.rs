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
