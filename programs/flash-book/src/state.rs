//! On-chain account types. These are the persistent state that lives in
//! the ER (when delegated) or on Solana mainnet (when undelegated).
//!
//! All structs use Anchor's `#[account]` and have the `discriminator: 8`
//! byte prefix Anchor expects. Production layouts use zero-copy where
//! the size justifies it; for v1 of this skeleton we use serde-style
//! Borsh which is simpler and sufficient under MAX_ORDERS_PER_BATCH.

use crate::constants::{
    MARK_HISTORY_LEN, MAX_FLP_QUOTE_LEVELS, MAX_ORDERS_PER_BATCH, MAX_POSITIONS_PER_TRADER,
    ORDER_BUFFER_CAP,
};
use crate::matcher::funding::FundingIndex;
use crate::matcher::lot::{BaseLots, Bps, Ticks};
use crate::matcher::order::{Order, Side};
use crate::matcher::vpin::VpinState;
use anchor_lang::prelude::*;

/// Per-market parameters. Set at market initialization, updated only via
/// governance. Mirrors `MarketParams` from the TypeScript reference.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize)]
pub struct MarketParams {
    pub tick_size: u64,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub min_base_lots: u64,

    pub taker_fee_bps: u32,
    pub maker_rebate_bps: u32,
    pub toxicity_tax_max_bps: u32,

    pub liq_penalty_bps: u32,
    pub maintenance_margin_ratio_bps: u32,
    pub initial_margin_ratio_bps: u32,
    pub max_leverage: u32,

    pub funding_rate_max_bps_per_sec: u32,
    pub funding_rate_k_bps: u32,

    pub oracle_band_bps: u32,

    pub flp_spread_base_bps: u32,
    pub flp_spread_alpha_bps: u32,
    pub flp_spread_beta_bps: u32,
    pub flp_spread_gamma_bps: u32,
    pub flp_spread_kappa_bps: u32,
    pub flp_spread_delta_bps: u32,    // realized-vol coefficient
    pub flp_inventory_lambda_bps: u32,
    pub flp_depth_floor_lots: u64,
    pub flp_max_growth_per_batch_bps: u32,
    pub flp_quote_levels: u8,

    pub vpin_bucket_size_lots: u64,
    pub vpin_ema_window: u32,

    pub twap_window: u8,
    pub batch_interval_ms: u32,

    /// Maximum age (in seconds) for an oracle price before it's rejected
    /// as stale. Mitigates the JELLY-style attack where attackers wait
    /// for an oracle gap to manipulate the mark.
    pub oracle_staleness_max_seconds: u32,

    /// Maximum oracle confidence interval as a fraction of price, in bps.
    /// E.g. 100 = 1%. When confidence widens beyond this, oracle updates
    /// are rejected (prevents using uncertain prices for liquidation).
    pub oracle_confidence_max_bps: u32,

    /// Per-trader maximum position size on this market, in base lots.
    /// 0 = unlimited. Prevents the POPCAT-style coordinated long buildup
    /// where a single attacker accumulates outsized concentrated risk.
    pub max_position_lots_per_trader: u64,
}

/// Top-level market state. One per pool market (e.g. SOL/USD, BTC/USD).
#[account]
#[derive(Debug)]
pub struct MarketAccount {
    pub authority: Pubkey,
    pub flp_pool: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub oracle_account: Pubkey,
    pub insurance_fund: Pubkey,
    pub bump: u8,
    pub status: u8,
    pub current_batch: u64,
    pub last_batch_ms: u64,
    pub oracle_price_ticks: u64,
    pub oracle_confidence: u64,
    pub oracle_published_at_unix_seconds: u64,
    pub mark_price_ticks: u64,
    pub cum_funding_index: i128,
    pub last_funding_rate_bps_per_sec: i64,
    pub vpin: VpinState,
    pub oi_long_lots: u64,
    pub oi_short_lots: u64,
    pub recent_clearing_prices: [u64; MARK_HISTORY_LEN],
    pub recent_clearing_count: u8,
    pub total_fees_collected: u64,
    pub total_toxicity_tax_collected: u64,
    pub total_liquidations: u64,
    pub params: MarketParams,
}

impl MarketAccount {
    pub const SEED: &'static [u8] = b"market";
    pub fn space() -> usize {
        // 8 (anchor disc) + struct fields. Borsh-conservative bound.
        // Actual size computed via std::mem::size_of for the constant fields,
        // but Anchor needs an explicit number. We pin a generous upper bound.
        8 + 1024
    }
}

#[account]
#[derive(Debug)]
pub struct PositionAccount {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub side: u8,
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub collateral_quote_lots: u64,
    pub cum_funding_index_at_entry: i128,
    pub realized_pnl_quote_lots: i64,
    pub funding_paid_quote_lots: i64,
    pub last_settlement_batch: u64,
}

impl PositionAccount {
    pub const SEED: &'static [u8] = b"position";
    pub fn space() -> usize {
        8 + 256
    }
    pub fn side_enum(&self) -> Side {
        if self.side == 0 { Side::Long } else { Side::Short }
    }
}

#[account]
#[derive(Debug)]
pub struct InsuranceFundAccount {
    pub authority: Pubkey,
    pub bump: u8,
    pub balance_quote_lots: u64,
    pub fee_contribution_bps: u32,
    pub toxicity_tax_contribution_bps: u32,
    pub liq_penalty_contribution_bps: u32,
    pub pause_threshold_quote_lots: u64,
    pub total_contributions: u64,
    pub total_payouts: u64,
}

impl InsuranceFundAccount {
    pub const SEED: &'static [u8] = b"insurance_fund";
    pub fn space() -> usize {
        8 + 128
    }
}

/// FLP pool exposure across markets. Single account; per-market sub-positions.
#[account]
#[derive(Debug)]
pub struct FlpExposureAccount {
    pub authority: Pubkey,
    pub bump: u8,
    pub total_capital_quote_lots: u64,
    pub realized_pnl: i64,
    pub markets_count: u8,
    pub per_market: [FlpMarketExposure; 16],
}

#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct FlpMarketExposure {
    pub market: Pubkey,
    pub side: u8, // 0 = long, 1 = short, 255 = empty
    pub size_lots: u64,
    pub entry_price_ticks: u64,
}

impl FlpExposureAccount {
    pub const SEED: &'static [u8] = b"flp_exposure";
    pub fn space() -> usize {
        8 + 32 + 1 + 8 + 8 + 1 + (16 * (32 + 1 + 8 + 8))
    }
}

/// Commit-reveal entry. Stored as a row in a per-market commit table.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct CommitRow {
    pub hash: [u8; 32],
    pub trader: Pubkey,
    pub bond: u64,
    pub committed_at_batch: u64,
    pub expire_at_batch: u64,
    pub valid: u8, // 0 = empty slot, 1 = active
}

/// Capacity of the per-market commit-reveal table. Sized to fit a single
/// `init` call within Solana's 10 KiB max account allocation.
pub const COMMIT_BUFFER_CAP: usize = 64;

#[account]
#[derive(Debug)]
pub struct CommitBufferAccount {
    pub market: Pubkey,
    pub bump: u8,
    pub head: u32,
    pub commits: [CommitRow; COMMIT_BUFFER_CAP],
}

impl CommitBufferAccount {
    pub const SEED: &'static [u8] = b"commit_buffer";
    pub fn space() -> usize {
        8 + 32 + 1 + 4 + (COMMIT_BUFFER_CAP * (32 + 32 + 8 + 8 + 8 + 1))
    }
}

/// Per-market order buffer: pending limit + revealed-taker orders for the
/// next `run_batch`. Cleared after each batch.
#[account]
#[derive(Debug)]
pub struct OrderBufferAccount {
    pub market: Pubkey,
    pub bump: u8,
    /// Number of valid orders in `slots[..head]`.
    pub head: u32,
    /// Monotonic sequence counter for FIFO ordering within a batch.
    pub seq_counter: u64,
    pub slots: [OrderSlot; ORDER_BUFFER_CAP],
}

/// Compact on-chain order representation. Mirrors `matcher::order::Order`
/// in the same fields but stored as a flat struct for Borsh serialization.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct OrderSlot {
    pub valid: u8,            // 0 = empty, 1 = active
    pub side: u8,             // 0 long, 1 short
    pub order_type: u8,       // 0 limit, 1 taker, 2 flp_virtual, 3 liq, 4 adl
    pub post_only: u8,        // 0 / 1
    pub seq: u64,
    pub id: u64,
    pub trader: Pubkey,
    pub size_lots: u64,
    pub limit_ticks: u64,
}

impl OrderBufferAccount {
    pub const SEED: &'static [u8] = b"order_buffer";
    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 4 head + 8 seq + (CAP × 4+8+8+32+8+8+1+1+1+1) padding-tolerant
        // OrderSlot: 1+1+1+1 + 8 + 8 + 32 + 8 + 8 = 68 bytes (Borsh; padding-free).
        8 + 32 + 1 + 4 + 8 + (ORDER_BUFFER_CAP * 68)
    }
}

/// Per-trader state. Holds collateral, last-settled funding marker, and
/// position-list pointers (Position PDAs are separate accounts; this is
/// a lightweight index).
#[account]
#[derive(Debug)]
pub struct TraderStateAccount {
    pub trader: Pubkey,
    pub bump: u8,
    pub collateral_quote_lots: u64,
    pub realized_pnl_quote_lots: i64,
    /// Number of open positions (each in its own Position PDA).
    pub open_positions: u8,
    /// Toxicity score in bps; updated post-fill. Used for taker-fee tier.
    pub toxicity_score_bps: i32,
    /// Per-batch order count (rate limit).
    pub orders_this_batch: u32,
    pub last_batch_seen: u64,
}

impl TraderStateAccount {
    pub const SEED: &'static [u8] = b"trader_state";
    pub fn space() -> usize {
        8 + 32 + 1 + 8 + 8 + 1 + 4 + 4 + 8 + 8
    }
}

const _: BaseLots = BaseLots(0);
const _: Ticks = Ticks(0);
const _: Bps = Bps(0);
const _: FundingIndex = 0i128;
const _: usize = MAX_POSITIONS_PER_TRADER;
const _: usize = MAX_FLP_QUOTE_LEVELS;
const _: usize = MAX_ORDERS_PER_BATCH;
const _: Order = Order {
    id: 0,
    trader: Pubkey::new_from_array([0; 32]),
    side: Side::Long,
    order_type: crate::matcher::order::OrderType::Limit,
    size: BaseLots(0),
    limit_price: Ticks(0),
    seq: 0,
    post_only: false,
};
