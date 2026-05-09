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

    /// Multi-oracle quorum maximum dispersion as bps of the median.
    /// E.g. 50 = if max-source - min-source > 0.5% of median, the update
    /// is rejected because oracles disagree. 0 = no dispersion check.
    /// Used by `update_oracle_quorum`.
    pub oracle_quorum_max_dispersion_bps: u32,

    /// Per-trader maximum position notional as bps of FLP capital.
    /// E.g. 10 = 0.1% of FLP capital. 0 = unlimited (only the absolute
    /// `max_position_lots_per_trader` gate applies). Computed at order
    /// placement against the trader's projected post-fill notional and
    /// the FLP's `total_capital_quote_lots`. Defends against cap-relative
    /// concentration risk that the absolute lots cap can't enforce when
    /// the pool grows or shrinks.
    pub max_position_ratio_bps: u32,

    /// Bps of liquidation penalty paid to the keeper that triggers the
    /// liquidation (a "tip" for being first to act). 0 = no incentive
    /// (operators rely on protocol-level keepers). Battle-tested CLOBs
    /// typically wire 1000-2000 bps (10-20% of penalty) to attract a
    /// competitive liquidator pool. The remainder of the penalty flows
    /// through the existing insurance-fund waterfall.
    pub liquidator_reward_bps: u32,

    /// Cooldown in slots between consecutive liquidate_position calls on
    /// the same position. Anti-cascade: prevents one underwater position
    /// from getting hit repeatedly in adjacent blocks (each costs the
    /// liquidatee a separate liquidation order + fee). Typical setting:
    /// 4-8 slots (~2-4 seconds). 0 = no cooldown (legacy behavior).
    pub liquidation_cooldown_slots: u32,

    /// Slots over which the liquidator reward grows from base to full.
    /// Dutch-style auction on the REWARD: first responders get a smaller
    /// reward, later responders progressively larger up to the full
    /// `liquidator_reward_bps`. 0 = reward is always full (legacy).
    /// Typical setting: 8-16 slots (~4-8 seconds). Encourages a
    /// competitive keeper pool to spread out instead of all racing the
    /// same block.
    pub liquidation_auction_duration_slots: u32,

    /// Drift-style JIT bonus: extra bps of rebate the maker earns when
    /// filling a JIT-tagged taker order (flag bit 3 on place_limit_order).
    /// 0 = JIT inactive. Typical setting: 5-20 bps (0.05-0.2% of notional)
    /// added on top of the base maker_rebate_bps. Encourages MMs to
    /// preferentially quote against tagged flow.
    pub jit_bonus_rebate_bps: u32,

    /// Hyperliquid-style affiliate program: when a taker has a referrer
    /// set on their TraderState, this many bps of the protocol's NET fee
    /// (post-rebate, post-discount) is credited to the referrer's
    /// TraderState collateral. 0 = referral program off. Typical: 1000-
    /// 2500 bps (10-25% of net fee).
    pub referrer_share_bps: u32,

    /// Builder code share — bps of net fee credited to the `builder` pubkey
    /// passed on the order. Frontends earn this for routing flow. 0 = off.
    /// Typical: 500-1500 bps (5-15% of net fee). Distinct from referral
    /// (referrer is per-trader; builder is per-order).
    pub builder_share_bps: u32,
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
    /// Slot at which this position was first detected as unhealthy by a
    /// `liquidate_position` call. 0 = healthy (or has never been
    /// liquidated). Used to compute the Dutch-auction reward curve and
    /// to enforce the per-position cooldown. Reset to 0 once the
    /// position closes (size_lots → 0).
    pub unhealthy_since_slot: u64,
    /// Slot at which the most recent liquidate_position call against
    /// this position landed. 0 = never liquidated. Used by the cooldown
    /// gate.
    pub last_liquidated_at_slot: u64,
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
    /// SPL mint for the protocol's quote currency (typically USDC). All
    /// collateral and FLP capital moves through this mint.
    pub quote_mint: Pubkey,
    /// Global protocol vault TokenAccount. Owned by the InsuranceFundAccount
    /// PDA (which signs transfers out via known seeds). Holds all trader
    /// collateral + FLP capital.
    pub quote_vault: Pubkey,
}

impl InsuranceFundAccount {
    pub const SEED: &'static [u8] = b"insurance_fund";
    pub fn space() -> usize {
        // 8 (disc) + 32 + 1 + 8 + 4 + 4 + 4 + 8 + 8 + 8 + 32 + 32 = 149.
        // Round up generously.
        8 + 192
    }
}

/// FLP pool exposure across markets and per-LP share accounting.
///
/// The pool's NAV (Net Asset Value) is `total_capital_quote_lots +
/// realized_pnl`. LPs own `shares` of `lp_shares_outstanding`; their
/// claim on NAV is `shares / lp_shares_outstanding`. Deposits mint
/// shares at the prevailing NAV/share price; withdrawals burn shares
/// for proportional NAV. This is the standard ERC-4626 vault model
/// adapted for Solana.
#[account]
#[derive(Debug)]
pub struct FlpExposureAccount {
    /// Protocol admin — manages FLP-level governance ops (initial endowment,
    /// future emergency pause). Distinct from LPs, who own shares.
    pub authority: Pubkey,
    pub bump: u8,
    /// Aggregate quote-lot deposits + maker rebate accrual. Used as one
    /// term of NAV; does NOT represent a single LP's claim.
    pub total_capital_quote_lots: u64,
    /// Cumulative realized P&L from FLP fills across all markets. Signed.
    /// The other term of NAV.
    pub realized_pnl: i64,
    pub markets_count: u8,
    /// Total shares issued across all LpPositionAccounts. NAV/share = NAV
    /// / lp_shares_outstanding when nonzero.
    pub lp_shares_outstanding: u64,
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
        // Original: 8 + 32 + 1 + 8 + 8 + 1 + 16*49 = 842.
        // Add 8 for lp_shares_outstanding. Round up generously.
        8 + 32 + 1 + 8 + 8 + 1 + 8 + (16 * (32 + 1 + 8 + 8))
    }

    /// Net Asset Value in quote lots. Returns i128 because realized_pnl can
    /// drive NAV negative in worst-case insolvency; callers should clamp
    /// or fail on negative NAV.
    pub fn nav(&self) -> i128 {
        (self.total_capital_quote_lots as i128) + (self.realized_pnl as i128)
    }
}

/// Native on-chain trigger order — Hyperliquid pattern. The trader pre-funds
/// rent on a `TriggerOrderAccount` PDA. Anyone (typically a keeper) can
/// `execute_trigger_order` once the oracle crosses `trigger_price_ticks` in
/// the configured direction; the chain inserts a regular limit order into
/// the market's buffer. Survives bot downtime — your stop fires even if
/// your MM bot is offline.
///
/// PDA seeds: [b"trigger", market, trader, trigger_id]. trigger_id is u8
/// (0..255) so each trader gets up to 256 active triggers per market.
#[account]
#[derive(Debug)]
pub struct TriggerOrderAccount {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub trigger_id: u8,
    /// Side of the resulting order when triggered (0 = long, 1 = short).
    /// For closing a long position: side = 1 (short to close). For closing
    /// a short: side = 0 (long to close).
    pub side: u8,
    /// `kind` encodes the comparison direction:
    ///   0 = trigger when oracle ≤ trigger_price  (stop-loss for longs,
    ///       take-profit for shorts)
    ///   1 = trigger when oracle ≥ trigger_price  (take-profit for longs,
    ///       stop-loss for shorts)
    pub kind: u8,
    /// Bit 0: reduce_only — execute only if the resulting order would
    ///        shrink the trader's position (no flip).
    /// Bit 1: active — set by `place_trigger_order`, cleared by
    ///        `execute_trigger_order` (no double-fire).
    pub flags: u8,
    pub size_lots: u64,
    /// Oracle price (in ticks) at which to fire.
    pub trigger_price_ticks: u64,
    /// Limit price for the resulting order. 0 = market-style (uses the
    /// current oracle ± slippage_bps configured at execute time, but for
    /// v1 we require an explicit non-zero limit to keep the matcher
    /// deterministic).
    pub limit_price_ticks: u64,
    pub created_at_slot: u64,
    /// 0 = never expires.
    pub expires_at_slot: u64,
}

impl TriggerOrderAccount {
    pub const SEED: &'static [u8] = b"trigger";
    pub const FLAG_REDUCE_ONLY: u8 = 1 << 0;
    pub const FLAG_ACTIVE: u8 = 1 << 1;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+1 + 8+8+8 + 8+8 = 117. Round up.
        8 + 128
    }
}

/// Native on-chain TWAP order — Hyperliquid pattern. The trader specifies
/// total size + slice size + interval; anyone (typically a keeper) calls
/// `execute_twap_slice` once per interval to insert one slice into the
/// market's order buffer. Slices stop firing when total filled OR end_slot
/// reached.
///
/// PDA seeds: [b"twap", market, trader, twap_id]. twap_id is u8 → up to
/// 256 active TWAPs per (trader, market) pair.
#[account]
#[derive(Debug)]
pub struct TwapOrderAccount {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub twap_id: u8,
    pub side: u8,
    pub flags: u8, // bit 0: active
    pub slice_size_lots: u64,
    /// Total size to execute across slices.
    pub total_size_lots: u64,
    /// Cumulative size successfully sliced into the buffer so far.
    pub size_executed_lots: u64,
    /// Limit price applied to every slice (max price for buys, min for
    /// sells). Slice rejects placement if exceeded.
    pub limit_price_ticks: u64,
    pub start_slot: u64,
    /// Minimum slots between successive slices.
    pub slot_interval: u64,
    /// 0 = no end. Otherwise last slot a slice may be placed in.
    pub end_slot: u64,
    pub last_slice_at_slot: u64,
}

impl TwapOrderAccount {
    pub const SEED: &'static [u8] = b"twap";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1 + 8+8+8+8+8+8+8+8 = 140. Round up.
        8 + 144
    }
}

/// Per-LP share holding. PDA seeded `[b"lp_position", lp.key()]`. Created
/// lazily on first deposit via `init_if_needed`.
#[account]
#[derive(Debug)]
pub struct LpPositionAccount {
    pub lp: Pubkey,
    pub bump: u8,
    /// Shares of FlpExposureAccount.lp_shares_outstanding owned by this LP.
    pub shares: u64,
    /// Cumulative quote-lot deposits over the lifetime of this LP. Cost
    /// basis for off-chain PnL display; does not affect on-chain math.
    pub total_deposited_quote_lots: u64,
    /// Cumulative quote-lot withdrawals.
    pub total_withdrawn_quote_lots: u64,
}

impl LpPositionAccount {
    pub const SEED: &'static [u8] = b"lp_position";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 8 + 8 + 8 = 65. Round up.
        8 + 96
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
    /// Fee tier discount in bps off the base taker fee. 0 = standard
    /// fees; e.g. 1000 = 10% discount. Set by `set_trader_fee_tier`
    /// (authority-only) based on off-chain 30-day rolling volume —
    /// universal pattern at every CEX (Binance, OKX, Bybit, Hyperliquid).
    pub fee_discount_bps: u32,
    /// Delegate authority. When non-default, the delegate may sign
    /// trader-bound instructions (place_limit_order, cancel_order,
    /// settle_funding, etc.) on the trader's behalf. Foundation for
    /// subaccount / portfolio-margin patterns:
    ///   • Master keypair holds funds, delegates trading authority to a
    ///     hot key (Hyperliquid / dYdX standard).
    ///   • Multi-sig "subaccount manager" can act on behalf of the
    ///     trader without holding their funds.
    /// Cleared by setting back to Pubkey::default(). The trader pubkey
    /// itself ALWAYS retains authority — delegate is additive, not
    /// exclusive (the trader can revoke at any time).
    pub delegate: Pubkey,
    /// Referrer pubkey. When non-default, on every fill where this trader
    /// is the taker, `market.params.referrer_share_bps` of the protocol's
    /// net fee revenue is credited to the referrer's TraderState
    /// collateral. Hyperliquid affiliate model. Pubkey::default() = no
    /// referrer (default for all new trader_state). Set once via
    /// `set_trader_referrer` — immutable after first set (anti-rotation
    /// griefing).
    pub referrer: Pubkey,
}

impl TraderStateAccount {
    pub const SEED: &'static [u8] = b"trader_state";
    pub fn space() -> usize {
        // 8 (disc) + 32 + 1 + 8 + 8 + 1 + 4 + 4 + 8 + 8 + 4 + 32 (delegate) +
        // 32 (referrer) = 150. Round up.
        8 + 160
    }

    /// Returns true if `signer` is authorized to act on this trader's
    /// behalf — either the trader themselves or a non-default delegate.
    pub fn is_authorized(&self, signer: &Pubkey) -> bool {
        signer == &self.trader || (self.delegate != Pubkey::default() && signer == &self.delegate)
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
