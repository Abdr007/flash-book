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

// ─── JIT liquidation offers v3 ──────────────────────────────────────
//
// A *maker* can pre-commit a "tighter than synthetic" close price to be
// used WHEN any underwater trader is liquidated on this market. When
// `liquidate_position_v2` fires, the matcher walks JIT offers first,
// picks the best price beating the synthetic `oracle ± liq_penalty`,
// and uses it as the close-order's limit price. The trader loses LESS
// collateral; the insurance fund draws LESS; the maker gets a
// guaranteed fill at a price they pre-committed.
//
// NO other on-chain DEX has this primitive — HL has private liquidations,
// Drift / dYdX use external keepers + insurance. JIT auctions = public
// pre-commit primitive where any maker can underbid the synthetic.
//
// Seeds: `[b"jit_liq_offer", market, maker, &nonce.to_le_bytes()]`.
// `nonce` is a u32 the maker picks so they can have multiple concurrent
// offers per market.
#[account]
#[derive(Debug)]
pub struct JitLiquidationOfferAccount {
    pub bump: u8,
    /// 0=will close LONG positions (acts as a BUYER from the long → bid),
    /// 1=will close SHORT positions (acts as a SELLER → ask). See ix
    /// docs for the close-side mapping.
    pub side: u8,
    pub _pad0: [u8; 2],
    pub nonce: u32,
    pub market: Pubkey,
    pub maker: Pubkey,
    /// `Pubkey::default()` means "any trader's liquidation on this market".
    pub target_trader: Pubkey,
    pub offer_price_ticks: u64,
    pub max_size_lots: u64,
    pub remaining_size_lots: u64,
    pub created_at_slot: u64,
    /// 0 = never expires; otherwise must be > current_slot at placement.
    pub expires_at_slot: u64,
}
impl JitLiquidationOfferAccount {
    pub const SEED: &'static [u8] = b"jit_liq_offer";
    pub fn space() -> usize {
        // 8 disc
        //   + 1 bump + 1 side + 2 pad + 4 nonce
        //   + 32 market + 32 maker + 32 target_trader
        //   + 8 offer_price + 8 max_size + 8 remaining_size
        //   + 8 created_at + 8 expires_at
        // = 8 + 152 = 160. Round up to 176.
        8 + 168
    }
}

// ─── Pyth oracle config (P0.1 — mainnet readiness) ──────────────────
//
// Per-market PDA that holds the Pyth feed ID + freshness bounds. Lives
// alongside the market rather than expanding `MarketParams` to avoid yet
// another account-layout migration. The `update_oracle_from_pyth` ix
// CPI-reads the Pyth `PriceUpdateV2` account and validates the feed_id
// matches this config before writing to `MarketAccount.oracle_*` fields.
//
// Seeds: `[b"oracle_config", market]`.
#[account]
#[derive(Debug)]
pub struct MarketOracleConfigAccount {
    pub bump: u8,
    /// 0 = legacy trusted `update_oracle` (devnet only). 1 = Pyth pull.
    /// Future: 2 = Switchboard, 3 = TWAP, etc.
    pub source: u8,
    pub _pad0: [u8; 6],
    pub market: Pubkey,
    /// The 32-byte Pyth feed identifier (e.g. SOL/USD on mainnet is
    /// `0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d`).
    pub pyth_price_feed_id: [u8; 32],
    pub max_staleness_seconds: u32,
    pub max_confidence_bps: u32,
    /// Tick decimal scaling. With our default tick = $0.001 and Pyth's
    /// typical -8 exponent, this is 3 (scale_exp = pyth.exponent + 3).
    /// Configurable per market because exotic feeds may use different
    /// exponents.
    pub tick_decimals: i8,
    pub _pad1: [u8; 7],
}
impl MarketOracleConfigAccount {
    pub const SEED: &'static [u8] = b"oracle_config";
    pub const SOURCE_TRUSTED: u8 = 0;
    pub const SOURCE_PYTH: u8 = 1;
    pub fn space() -> usize {
        // 8 disc
        //   + 1 bump + 1 source + 6 pad
        //   + 32 market + 32 feed_id
        //   + 4 + 4 max_staleness/conf
        //   + 1 tick_decimals + 7 pad
        // = 8 + 88 = 96. Round up to 128.
        8 + 120
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jit_offer_seed_is_stable() {
        assert_eq!(JitLiquidationOfferAccount::SEED, b"jit_liq_offer");
    }

    #[test]
    fn jit_offer_space_is_at_least_layout_size() {
        // Underlying bytes (excluding the 8-byte Anchor disc):
        //   1 + 1 + 2 + 4 + 32 + 32 + 32 + 8*5 = 152 bytes
        let layout_body = 1 + 1 + 2 + 4 + 32 + 32 + 32 + 8 * 5;
        assert!(JitLiquidationOfferAccount::space() >= 8 + layout_body);
    }

    #[test]
    fn jit_offer_pda_seed_distinct_from_v3_others() {
        // Confirm the JIT seed prefix doesn't collide with any sibling V3 seed
        // (regression: someone reusing `trigger_v3` etc).
        let jit = JitLiquidationOfferAccount::SEED;
        assert_ne!(jit, TriggerOrderAccountV3::SEED);
        assert_ne!(jit, TwapOrderAccountV3::SEED);
        assert_ne!(jit, IcebergOrderAccountV3::SEED);
        assert_ne!(jit, VaultAccountV3::SEED);
        assert_ne!(jit, VaultPositionAccountV3::SEED);
        assert_ne!(jit, FlpExposurePerMarketAccountV3::SEED);
        assert_ne!(jit, FlpPositionAccountV3::SEED);
    }
}
