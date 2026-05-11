//! Flash Book V3 account types — merged in from the (now-deleted)
//! flash-book-orders / flash-book-flp / flash-book-vaults wrapper
//! programs. All types live under `flash_book` program ID; PDAs use
//! distinct seed prefixes (`trigger_v3`, `vault_v3`, etc.) so they
//! coexist alongside legacy v1/v2 types without seed collision.

use anchor_lang::prelude::*;

// ─── Trigger orders v3 ──────────────────────────────────────────────

/// V3 trigger order. Seeds: `[b"trigger_v3", market, trader, trigger_id]`.
/// Distinct from legacy `[b"trigger", ...]` so legacy + v3 triggers can
/// coexist during a migration window.
#[account]
#[derive(Debug)]
pub struct TriggerOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub trigger_id: u8,
    pub side: u8,
    pub kind: u8,
    pub flags: u8,
    pub size_lots: u64,
    pub trigger_price_ticks: u64,
    pub limit_price_ticks: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
}

impl TriggerOrderAccountV3 {
    pub const SEED: &'static [u8] = b"trigger_v3";
    pub const FLAG_REDUCE_ONLY: u8 = 1 << 0;
    pub const FLAG_ACTIVE: u8 = 1 << 1;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+1 + 8+8+8+8+8 = 117. Round to 128.
        8 + 128
    }
}

/// V3 TWAP order. Seeds: `[b"twap_v3", market, trader, twap_id]`.
#[account]
#[derive(Debug)]
pub struct TwapOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub twap_id: u8,
    pub side: u8,
    pub flags: u8, // bit 0: active
    pub slice_size_lots: u64,
    pub total_size_lots: u64,
    pub size_executed_lots: u64,
    pub limit_price_ticks: u64,
    pub start_slot: u64,
    pub slot_interval: u64,
    pub end_slot: u64,
    pub last_slice_at_slot: u64,
}
impl TwapOrderAccountV3 {
    pub const SEED: &'static [u8] = b"twap_v3";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        8 + 144
    }
}

/// V3 iceberg order. Seeds: `[b"iceberg_v3", market, trader, iceberg_id]`.
#[account]
#[derive(Debug)]
pub struct IcebergOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub iceberg_id: u8,
    pub side: u8,
    pub flags: u8, // bit 0: active
    pub _pad0: [u8; 4],
    pub limit_ticks: u64,
    pub total_size_lots: u64,
    pub remaining_lots: u64,
    pub displayed_size_lots: u64,
    pub child_order_seq: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
}
impl IcebergOrderAccountV3 {
    pub const SEED: &'static [u8] = b"iceberg_v3";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        8 + 128
    }
}

// ─── Vaults v3 ──────────────────────────────────────────────────────

/// V3 vault account. Seeds: `[b"vault_v3", strategist, vault_id]`.
#[account]
#[derive(Debug)]
pub struct VaultAccountV3 {
    pub strategist: Pubkey,
    pub bump: u8,
    pub vault_id: u8,
    pub accept_deposits: u8,
    pub _pad0: u8,
    pub name: [u8; 32],
    pub perf_fee_bps: u32,
    pub shares_outstanding: u64,
    /// Cumulative gross deposits over the vault's lifetime (informational).
    pub total_capital_quote_lots: u64,
    /// HWM of NAV-per-share, scaled by USD_UNIT (1_000_000). 0 = bootstrap.
    pub hwm_nav_per_share_u64x6: u64,
    pub last_perf_settlement_unix: u64,
    pub total_perf_shares_minted: u64,
}
impl VaultAccountV3 {
    pub const SEED: &'static [u8] = b"vault_v3";
    pub fn space() -> usize {
        8 + 144
    }
}

/// V3 vault depositor position. Seeds: `[b"vault_position_v3", vault, depositor]`.
#[account]
#[derive(Debug, Default)]
pub struct VaultPositionAccountV3 {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub bump: u8,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
}
impl VaultPositionAccountV3 {
    pub const SEED: &'static [u8] = b"vault_position_v3";
    pub fn space() -> usize {
        8 + 112
    }
}

// ─── Per-market FLP v3 ──────────────────────────────────────────────

/// Per-market FLP exposure. Replaces the singleton's per_market[] array
/// for independent ER-delegation per market.
#[account]
#[derive(Debug)]
pub struct FlpExposurePerMarketAccountV3 {
    pub market: Pubkey,
    pub authority: Pubkey,
    pub bump: u8,
    pub side: u8, // 0=long, 1=short, 255=empty
    pub _pad0: [u8; 6],
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub total_capital_quote_lots: u64,
    pub realized_pnl: i64,
    pub lp_shares_outstanding: u64,
}
impl FlpExposurePerMarketAccountV3 {
    pub const SEED: &'static [u8] = b"flp_per_market";
    pub fn space() -> usize {
        8 + 128
    }
}

/// Per-LP, per-market FLP shares balance.
#[account]
#[derive(Debug)]
pub struct FlpPositionAccountV3 {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub bump: u8,
    pub _pad: [u8; 7],
    pub shares: u64,
}
impl FlpPositionAccountV3 {
    pub const SEED: &'static [u8] = b"flp_position_v3";
    pub fn space() -> usize {
        8 + 96
    }
}
