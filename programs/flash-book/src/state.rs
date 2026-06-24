//! On-chain account types. These are the persistent state that lives in
//! the ER (when delegated) or on Solana mainnet (when undelegated).
//!
//! All structs use Anchor's `#[account]` and have the `discriminator: 8`
//! byte prefix Anchor expects. Production layouts use zero-copy where
//! the size justifies it; for v1 of this skeleton we use serde-style
//! Borsh which is simpler and sufficient under MAX_ORDERS_PER_BATCH.

use crate::constants::{
    MARK_HISTORY_LEN, MAX_FLP_QUOTE_LEVELS, MAX_ORDERS_PER_BATCH, MAX_POSITIONS_PER_TRADER,
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
    /// Maker fee/rebate rate. SIGNED — positive = rebate paid to maker
    /// (legacy semantics, MM incentive); negative = fee charged to maker
    /// (low-tier retail). Crossing the sign boundary is supported on
    /// the multi-tier fee table (FeeTier rows can mix signs across
    /// volume tiers — e.g. tier 0 = -10 (10 bps maker fee), tier 5 =
    /// +5 (5 bps rebate)). u32 → i32 widening preserves byte size
    /// (4 bytes either way) so MarketParams layout is unchanged.
    pub maker_rebate_bps: i32,
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

    /// HIP-3 / permissionless-deployer share. When the market was created
    /// permissionlessly, `MarketAccount.creator` is the deployer pubkey and
    /// this is the share of net fees credited to them. 0 = no creator
    /// share (typical for protocol-deployed core markets). Typical for
    /// permissionless: 1000-3000 bps. Stacks with referrer + builder
    /// (the *protocol* takes the residual into the insurance fund).
    pub creator_share_bps: u32,

    /// Pre-launch market flag. When true, this market is trading a
    /// pre-TGE asset whose oracle is supplied by `update_oracle` (manual /
    /// quorum) rather than Pyth. Off-chain UIs show a "PRE-LAUNCH"
    /// badge. Hyperliquid pattern: enables price discovery on a perp
    /// before its spot exists. On-chain semantics are identical to a
    /// regular market — governance is expected to set tighter limits
    /// (lower max_leverage, lower max_position_lots_per_trader) at init.
    pub is_pre_launch: bool,

    /// Maximum gross open interest in base lots (per side: long lots OR
    /// short lots, whichever is larger). 0 = unlimited. Acts as a hard
    /// circuit breaker against runaway exposure on a single market —
    /// new orders that would push OI past the cap are rejected at
    /// place_limit_order intake. Distinct from `max_position_lots_per_trader`
    /// (per-trader) and `max_position_ratio_bps` (per-trader as % of FLP);
    /// this caps the WHOLE-MARKET aggregate. Typical: scaled with FLP
    /// capital × leverage so worst-case insurance draw stays bounded.
    pub max_oi_base_lots: u64,

    /// Maximum allowed mark-price change per batch in bps. 0 = unlimited
    /// (legacy / pre-launch markets often run open). When set, the
    /// matcher clamps the post-batch mark to ±this fraction of the
    /// previous mark. Hyperliquid-style anti-flash-crash defense:
    /// prevents a single thin-liquidity batch (or oracle spike that
    /// passed the band gate) from setting an outlier mark that would
    /// liquidate a swathe of healthy positions on the next assess.
    /// Typical: 200-1000 bps (2%-10% per batch ≈ per 50ms).
    pub mark_change_max_bps: u32,

    /// CME-style concentration margin tier. When a position's size_lots
    /// crosses `concentration_threshold_lots`, its effective maintenance
    /// margin becomes `maintenance_margin_ratio_bps +
    /// concentration_extra_mmr_bps`. Penalises whales whose size is
    /// harder to liquidate without market impact. 0 threshold = tier
    /// disabled (single-MMR for the whole market). Smarter than HL's
    /// flat per-market MMR.
    /// Typical: threshold sized to 1-5% of FLP capital; extra 100-500 bps.
    pub concentration_threshold_lots: u64,
    pub concentration_extra_mmr_bps: u32,

    /// TWAP window length (in batches) for the funding-premium dampener.
    /// 0 = disabled (legacy single-tick premium). When > 0, the funding
    /// rate uses the average of the last N batches' (mark - oracle)
    /// premium instead of the instantaneous one — kills funding spikes
    /// from microbursts of toxic flow that move the mark for one batch.
    /// HL uses single-tick premium; this is mathematically smarter.
    /// Capped at MARK_HISTORY_LEN (16). Typical: 4-8 batches.
    pub funding_premium_twap_window: u8,

    /// Funding-per-period cap (anti-gouge). When non-zero, the absolute
    /// cumulative funding paid in a rolling window of
    /// `funding_period_seconds` cannot exceed `funding_per_period_max_bps`
    /// (in bps of position notional). Once the cap is hit, the funding
    /// rate is scaled down for the remainder of the window so the
    /// total stays at or under the cap. Smarter than HL where extended
    /// one-way funding can drain a position without a daily ceiling.
    /// Bookkeeping fields live on MarketAccount (period_*).
    /// 0 = disabled (legacy / HL-equivalent).
    pub funding_per_period_max_bps: u32,
    /// Period length for the funding cap, in seconds. Typical: 86_400
    /// (24h). Ignored if `funding_per_period_max_bps == 0`.
    pub funding_period_seconds: u32,

    /// Bootstrap-period batches for permissionless markets. Within the
    /// first N batches after a market is initialized, all per-trader
    /// and whole-market position/OI caps are tightened by a factor of
    /// 4 to defend against snipers in the price-discovery window. After
    /// `current_batch >= bootstrap_period_batches`, normal caps apply.
    /// 0 = disabled (legacy markets and protocol-curated deploys).
    pub bootstrap_period_batches: u32,

    /// Symmetric-OI funding dampener. When true, the funding rate is
    /// scaled by the OI imbalance:
    ///   skew_bps = |oi_long − oi_short| × 10_000 / (oi_long + oi_short)
    ///   dampened_rate = rate × skew_bps / 10_000
    /// When the book is balanced (skew = 0), funding is fully dampened
    /// (zero) — no reason to drain anyone since no side dominates.
    /// When the book is one-sided (skew = 10_000 = 100%), funding is
    /// at full strength to incentivise correction. HL charges full
    /// premium-driven funding even with balanced OI; this is a
    /// genuinely smarter signal of "actual incentive needed."
    /// Default false = HL-equivalent.
    pub funding_oi_dampening: bool,

    // ─── V3 mark-price engine ────────────────────────────────────────
    /// EMA weight (in bps) applied to a fresh fill price when blending
    /// it into `mark_price_ticks` inside `apply_fill`.
    ///     new_mark = alpha * fill + (1 - alpha) * old_mark
    /// 0 = mark is never updated by fills (rely entirely on
    /// `settle_mark` for resync — appropriate for markets that prefer
    /// strict oracle-anchored mark).
    /// Typical setting: 2_000 (20% weight on each fill — dampens
    /// outlier fills but still tracks the tape). Capped at BPS_DENOM.
    pub mark_ema_alpha_bps: u32,
    /// Maximum allowed per-fill mark move in bps, clamped (not rejected).
    /// If the EMA-blended new_mark differs from the prior mark by more
    /// than this fraction, the move is clamped to ±this so a single
    /// outlier fill cannot flash-crash the mark. 0 = unlimited.
    /// Typical: 500 bps = 5%.
    pub mark_max_change_bps: u32,
    /// Minimum number of slots between consecutive permissionless
    /// `settle_mark` calls. Acts as a rate-limit so a single sequencer
    /// can't spam settles. 0 = no rate limit. Typical: 10 slots ≈ 4 s.
    pub mark_settle_min_slots: u32,
    /// When |mark - oracle| / oracle exceeds this (in bps), every
    /// mark-update path emits a `MarkPriceDriftEvent` so off-chain
    /// observers can nudge `settle_mark`. 0 = drift alerts disabled.
    /// Typical: 100 bps = 1%.
    pub drift_alert_bps: u32,
}

/// Top-level market state. One per pool market (e.g. SOL/USD, BTC/USD).
#[account]
#[derive(Debug)]
pub struct MarketAccount {
    pub authority: Pubkey,
    /// Curated-creator pubkey. Always zeroed by `initialize_market`
    /// (markets are authority-gated in V3 — no permissionless creator
    /// share is paid out). When non-default (set via a future
    /// authority-controlled migration ix), every fill on this market
    /// emits a CreatorFeeOwedEvent crediting the creator with
    /// `params.creator_share_bps` of net fee.
    pub creator: Pubkey,
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
    /// Period bookkeeping for the funding-per-period cap. The
    /// `period_funding_paid_abs_bps` accumulator advances with every
    /// `settle_funding` call; resets at the start of each new period.
    /// `period_started_at_unix == 0` means uninitialised — first
    /// settlement seeds it from the current clock.
    pub period_started_at_unix: u64,
    pub period_funding_paid_abs_bps: u64,
    /// Slot of the most recent `settle_mark` call. 0 = never settled.
    /// Used to enforce `params.mark_settle_min_slots` rate-limit.
    pub last_mark_settle_slot: u64,
    pub params: MarketParams,
    /// Authorized fill-settlement signer for `apply_fill` / `apply_flp_fill`.
    ///
    /// SECURITY (C-1): this is DELIBERATELY decoupled from `authority`.
    /// `authority` is zeroed by the authority-burn ladder
    /// (`renounce_market_authority`), but settlement must keep working
    /// after decentralization — so the writer that can post fills is
    /// gated by this dedicated, separately-rotatable key instead.
    ///
    /// Set to `authority` at init; rotate via `set_market_sequencer`
    /// (authority-gated, so do it BEFORE burning authority). Markets
    /// created before this field existed read it back as the zero pubkey
    /// (additive-migration trailing-zero convention) — which is
    /// UNSIGNABLE, so `apply_fill` safely halts (refuses forgery) until
    /// the authority calls `set_market_sequencer`. Fail-closed by design.
    pub sequencer: Pubkey,
}

impl MarketAccount {
    pub const SEED: &'static [u8] = b"market";
    pub fn space() -> usize {
        // 8 (anchor disc) + struct fields. Borsh-conservative bound.
        // Actual size computed via std::mem::size_of for the constant fields,
        // but Anchor needs an explicit number. We pin a generous upper bound.
        // V3 added `last_mark_settle_slot` (8 B) + four new MarketParams
        // u32 fields (16 B) → bumped to 1152 for headroom.
        8 + 1152
    }
}

/// Hyperliquid-style multi-tier MMR table — wave 20a.
///
/// Per-market account that holds up to MAX_LEVERAGE_TIERS rungs of
/// `(min_notional_quote_lots, mmr_bps)`. A position's effective MMR is
/// resolved via `matcher::risk::tiered_mmr_bps(market.maintenance_margin_bps,
/// tiers, position_notional)`.
///
/// OPTIONAL: a market without this PDA falls back to the existing 2-tier
/// model (baseline + concentration_extra_mmr). Markets that need the full
/// HL leverage curve (BTC, ETH, ALT majors) can opt in via
/// `init_market_leverage_tiers`.
///
/// Authority-gated: only the market authority can init / update.
#[account]
#[derive(Debug, Default)]
pub struct MarketLeverageTiersAccount {
    pub market: Pubkey,
    pub bump: u8,
    /// Number of valid tiers in `tiers[..tier_count]`. Slots beyond
    /// `tier_count` are zero-init padding.
    pub tier_count: u8,
    pub _pad0: [u8; 6],
    /// Sorted ascending by `min_notional_quote_lots`. Validated at
    /// init/update — see `lib.rs:init_market_leverage_tiers`.
    pub tiers: [LeverageTier; MAX_LEVERAGE_TIERS],
}

/// One rung of the leverage tier table.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct LeverageTier {
    pub min_notional_quote_lots: u64,
    pub mmr_bps: u32,
    pub _pad: [u8; 4],
}

/// HL uses up to 6 tiers per asset; 8 gives us headroom for very large
/// markets (e.g. BTC could justify 10-20 tiers in extreme cases, but 8
/// covers the practical envelope).
pub const MAX_LEVERAGE_TIERS: usize = 8;

impl MarketLeverageTiersAccount {
    pub const SEED: &'static [u8] = b"leverage_tiers";
    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 1 count + 6 pad +
        //   MAX × (8 + 4 + 4) = 8 × 16 = 128
        8 + 32 + 1 + 1 + 6 + (MAX_LEVERAGE_TIERS * 16)
    }
}

/// WAVE 22 — Multi-tier fee table. Global per-program, authority-set.
/// Replaces the legacy single `TraderStateAccount.fee_discount_bps`
/// pattern (where authority manually sets a per-trader discount) with
/// the HL / Binance / dYdX standard volume-tier model:
///
///   • One global tier table (this account, PDA `[b"fee_tiers"]`).
///   • Each tier specifies a `min_volume_quote_lots` threshold + the
///     effective `maker_rebate_bps` and `taker_fee_bps` for traders
///     that have crossed it within the rolling window.
///   • Trader's window volume is tracked on `TraderStateAccount.
///     volume_30d_quote_lots`, credited on every fill (maker + taker).
///   • `resolve_fee_tier(volume, tiers)` picks the highest tier the
///     trader's cumulative window volume satisfies.
///
/// Coexists with the legacy `fee_discount_bps`: tier's bps SUPERSEDE
/// market default; `fee_discount_bps` then applies as a further
/// percentage discount (so promo / referral codes can stack on top
/// of the base tier rate).
///
/// Authority-gated: only the protocol authority can init / update.
#[account]
#[derive(Debug, Default)]
pub struct FeeTiersAccount {
    pub authority: Pubkey,
    pub bump: u8,
    pub tier_count: u8,
    pub _pad0: [u8; 6],
    /// Length of the volume-tracking window in slots. Crossing this
    /// boundary on the next apply_fill resets the trader's
    /// `volume_30d_quote_lots` to 0 and re-anchors the window. HL
    /// pattern uses 14 days ≈ 3_024_000 slots @ 0.4s. Authority sets
    /// this to match their preferred review cadence.
    pub volume_window_slots: u64,
    /// Sorted ascending by `min_volume_quote_lots`. Tier 0 (volume 0
    /// threshold) is the default for new traders. Validated at
    /// init/update — see `lib.rs:init_fee_tiers`.
    pub tiers: [FeeTier; MAX_FEE_TIERS],
}

/// One rung of the fee-tier table. `min_volume_quote_lots` is the
/// rolling-window cumulative notional (in quote lots) that the trader
/// must have traded to qualify. Higher tiers offer LOWER taker fees
/// and HIGHER maker rebates.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize, Default)]
pub struct FeeTier {
    pub min_volume_quote_lots: u64,
    /// SIGNED maker rate. Positive = rebate paid TO maker (legacy
    /// MM-incentive semantics); negative = fee charged FROM maker
    /// (low-tier retail). Validated to be monotone non-decreasing
    /// across tiers (a higher-volume trader's maker treatment is
    /// never worse than a lower-volume trader's).
    pub maker_rebate_bps: i32,
    pub taker_fee_bps: u32,
}

/// HL has 7 tiers (Wood, Bronze, Silver, Gold, Platinum, Diamond, Diamond+).
/// Binance has 9 (VIP 0-9). 10 covers both with headroom.
pub const MAX_FEE_TIERS: usize = 10;

impl FeeTiersAccount {
    pub const SEED: &'static [u8] = b"fee_tiers";
    pub fn space() -> usize {
        // 8 disc + 32 authority + 1 bump + 1 count + 6 pad +
        //   8 window_slots + MAX × (8 + 4 + 4) = 10 × 16 = 160
        8 + 32 + 1 + 1 + 6 + 8 + (MAX_FEE_TIERS * 16)
    }
}

/// Per-(market, trader) open position.
///
/// ─── ISOLATED MARGIN DESIGN (Phase 2) ────────────────────────────────
/// `collateral_quote_lots` is currently a defined-but-unused field. It
/// is reserved as the on-chain marker for isolated-margin positions:
///
///   collateral_quote_lots == 0  → cross margin (default, current
///                                 behavior). Position is backed by the
///                                 trader's pooled `TraderStateAccount
///                                 .collateral_quote_lots`.
///
///   collateral_quote_lots  > 0  → isolated margin. The position is
///                                 backed ONLY by this amount; the
///                                 trader's pooled collateral is
///                                 insulated from this position's
///                                 liquidation.
///
/// Phase 2 (separate, audited commit) wires the marker through:
///
///   1. `assess_margin_fn` (matcher/risk.rs) — splits the trader's
///      position set into cross and isolated, evaluates each isolated
///      position against its own collateral, and only includes cross
///      positions in the pooled-collateral assessment.
///   2. `liquidate_position_v2` — when liquidating an isolated
///      position, the penalty + liquidator reward come out of
///      `position.collateral_quote_lots` first, then the insurance
///      fund covers any shortfall (the trader's main pool is never
///      touched — that's the whole point of isolated).
///   3. New ixs `set_position_isolated(amount)` and
///      `set_position_cross()` that transfer collateral between
///      `TraderState.collateral_quote_lots` and
///      `PositionAccount.collateral_quote_lots`, gated by a
///      post-transfer health check on BOTH the cross set and the
///      isolated position.
///
/// Until Phase 2 lands, no on-chain logic writes to or reads from
/// this field. Off-chain code may rely on this field staying 0 for
/// every existing position.
#[account(zero_copy)]
#[derive(Debug)]
pub struct PositionAccount {
    // CU Phase 1 — zero-copy Pod layout. The i128 is placed FIRST at a
    // 16-byte-aligned offset so the byte layout is identical on host
    // (i128 align 16) and SBF (align 8): no implicit padding on either,
    // which bytemuck `Pod` requires. Tail padded to a 16-byte multiple.
    /// i128 cumulative funding index at entry, stored as little-endian
    /// bytes. A native i128 would give the struct 16-byte alignment, but
    /// Anchor zero-copy data begins at disc offset +8 (8-aligned only), so
    /// `bytemuck::from_bytes` would panic. Access via `cum_funding_index()` /
    /// `set_cum_funding_index()`.
    pub cum_funding_index_at_entry: [u8; 16],
    pub trader: Pubkey,
    pub market: Pubkey,
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub collateral_quote_lots: u64,
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
    /// Per-position leverage cap (set by trader via `set_position_leverage`).
    /// 0 = use the market's `params.max_leverage`. Otherwise capped at
    /// `min(params.max_leverage, leverage_cap)` during margin checks.
    /// Hyperliquid pattern: lets risk-conscious traders limit their
    /// exposure on a per-position basis without affecting other positions.
    /// Validated at set time: cap ∈ [1, market.max_leverage].
    pub leverage_cap: u32,
    pub bump: u8,
    pub side: u8,
    pub _pad: [u8; 2],
}

impl PositionAccount {
    pub const SEED: &'static [u8] = b"position";
    pub fn space() -> usize {
        8 + std::mem::size_of::<Self>()
    }

    /// Cumulative funding index at entry as `i128` (stored LE-bytes to keep
    /// the account 8-aligned for Anchor zero-copy).
    #[inline]
    pub fn cum_funding_index(&self) -> i128 {
        i128::from_le_bytes(self.cum_funding_index_at_entry)
    }
    #[inline]
    pub fn set_cum_funding_index(&mut self, v: i128) {
        self.cum_funding_index_at_entry = v.to_le_bytes();
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
    /// OCO (One-Cancels-the-Other) partner trigger PDA. Default = no
    /// link. When non-default, executing OR cancelling THIS trigger
    /// also marks the partner inactive (one fires, the other dies).
    /// Set by `place_bracket_order` to wire a TP+SL pair atomically.
    /// The partner account is passed via the optional `oco_pair`
    /// account on `execute_trigger_order` / `cancel_trigger_order`.
    pub oco_pair: Pubkey,
    /// Trailing-stop offset in bps. 0 = static trigger (legacy). When
    /// non-zero, the trigger price RATCHETS in the favourable direction
    /// as the oracle moves: kind=0 (fire on ≤) tracks the oracle MAX
    /// minus offset; kind=1 (fire on ≥) tracks oracle MIN plus offset.
    /// Hyperliquid trailing-stop pattern. Permissionless `update_trailing_stop`
    /// keepers ratchet the trigger price; on fire, the existing
    /// execute_trigger_order path runs unchanged.
    pub trailing_offset_bps: u32,
    /// Best (most-favorable-to-trader) oracle price observed since
    /// trigger placement. Used as the anchor for trailing math:
    ///   kind=0 (sl-for-long): trigger = best_price × (1 - offset_bps)
    ///   kind=1 (sl-for-short): trigger = best_price × (1 + offset_bps)
    /// 0 = unset (first update_trailing_stop initialises from current oracle).
    pub trailing_anchor_ticks: u64,
    /// Phase 2f — TraderState sub-account index this trigger fires
    /// against. Layout-compatible: pre-Phase-2f accounts read this back
    /// as 0 (main) from the trailing zeros of the allocated `space()`.
    /// `execute_trigger_order_v2` writes this into the synthetic
    /// RestingOrderV2.sub_index so the resulting fill routes to the
    /// right TraderState.
    pub sub_index: u8,
}

impl TriggerOrderAccount {
    pub const SEED: &'static [u8] = b"trigger";
    pub const FLAG_REDUCE_ONLY: u8 = 1 << 0;
    pub const FLAG_ACTIVE: u8 = 1 << 1;
    /// Bracket leg flag — set by `place_bracket_order`. Informational;
    /// OCO behaviour keys off `oco_pair != Pubkey::default()`.
    pub const FLAG_BRACKET_LEG: u8 = 1 << 2;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+1 + 8+8+8 + 8+8 + 32 (oco)
        // + 4 (trailing_offset_bps) + 8 (trailing_anchor_ticks) = 161.
        // Round to 192.
        8 + 192
    }
}

/// Native on-chain ICEBERG order — Hyperliquid pattern. Hides total
/// size by displaying only `displayed_size_lots` at a time. When the
/// visible child fills, a permissionless `replenish_iceberg` keeper
/// inserts the next chunk of `displayed_size_lots` (or the residual)
/// from the hidden reservoir at the same `limit_ticks`.
///
/// PDA seeds: [b"iceberg", market, trader, iceberg_id]. iceberg_id is
/// u8 → up to 256 active icebergs per (trader, market) pair.
#[account]
#[derive(Debug)]
pub struct IcebergOrderAccount {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub iceberg_id: u8,
    pub side: u8,
    /// bit 0 = active. Cleared when remaining_lots == 0 OR cancelled.
    pub flags: u8,
    /// Phase 2f — repurposes the first byte of the prior `_pad0: [u8; 5]`.
    /// Layout-compatible: pre-Phase-2f accounts have this byte as 0
    /// (main TraderState) by virtue of the zero-initialised allocation.
    pub sub_index: u8,
    pub _pad0: [u8; 4],
    pub limit_ticks: u64,
    pub total_size_lots: u64,
    /// Lots not yet displayed in the orderbook (the hidden reservoir).
    pub remaining_lots: u64,
    /// Size of each visible chunk. Last chunk may be smaller (residual).
    pub displayed_size_lots: u64,
    /// Sequence of the current child order in the OrderBuffer. 0 = no
    /// active child (about to replenish or fully drained).
    pub child_order_seq: u64,
    pub created_at_slot: u64,
    /// 0 = never expires.
    pub expires_at_slot: u64,
}

impl IcebergOrderAccount {
    pub const SEED: &'static [u8] = b"iceberg";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+5 + 8+8+8+8+8+8+8 = 137. Round to 168.
        8 + 168
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
    /// Phase 2f — TraderState sub-account index. Every slice's
    /// synthetic RestingOrderV2 carries this so fills route to the
    /// right TraderState. Layout-compatible (trailing zeros = 0 = main).
    pub sub_index: u8,
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

/// Per-trader state. Holds collateral, last-settled funding marker, and
/// position-list pointers (Position PDAs are separate accounts; this is
/// a lightweight index).
#[account(zero_copy)]
#[derive(Debug)]
pub struct TraderStateAccount {
    // ── CU Phase 1: zero-copy Pod layout ─────────────────────────────────
    // Fields are ordered by DESCENDING alignment (Pubkey[u8;32] → i64/u64 →
    // i32/u32 → u8 → tail pad) so the struct has NO implicit padding and is
    // `bytemuck::Pod` for `#[account(zero_copy)]`. Only u8/u32/i32/u64/i64
    // (NO u128) so host and SBF alignments match. Total body = 192 bytes.
    pub trader: Pubkey,
    /// Delegate authority. When non-default, the delegate may sign
    /// trader-bound instructions on the trader's behalf (subaccount /
    /// portfolio-margin patterns). Cleared via Pubkey::default(). The
    /// trader ALWAYS retains authority — delegate is additive.
    pub delegate: Pubkey,
    /// Referrer pubkey (Hyperliquid affiliate model). Set once via
    /// `set_trader_referrer` — immutable after first set.
    pub referrer: Pubkey,
    /// Approved builder pubkey (Hyperliquid builder-codes). Set/rotated via
    /// `set_trader_builder`; the trader's CAP keeps builders bounded.
    pub builder: Pubkey,

    pub collateral_quote_lots: u64,
    pub realized_pnl_quote_lots: i64,
    pub last_batch_seen: u64,
    /// Wave 22: rolling notional in the current volume window (quote lots,
    /// maker + taker). Reset on window expiry; drives `resolve_fee_tier`.
    pub volume_30d_quote_lots: u64,
    /// Wave 22: slot the current volume window opened.
    pub volume_window_start_slot: u64,

    /// Toxicity score in bps; updated post-fill. Used for taker-fee tier.
    pub toxicity_score_bps: i32,
    /// Per-batch order count (rate limit).
    pub orders_this_batch: u32,
    /// Fee tier discount in bps off the base taker fee (set by
    /// `set_trader_fee_tier`, authority-only).
    pub fee_discount_bps: u32,
    /// Max fee share (bps of net fee) the trader authorized the builder
    /// to take; the on-chain emit clamps builder_share_bps by this.
    pub builder_max_fee_share_bps: u32,

    pub bump: u8,
    /// Number of open positions (each in its own Position PDA).
    pub open_positions: u8,
    /// Phase 2f — sub-account index. `0` = main; `1..=255` = sub. Set at
    /// `open_trader_state` (0) / `open_trader_sub_account` (sub_index).
    pub sub_index: u8,
    pub _pad: [u8; 5],
}

impl TraderStateAccount {
    pub const SEED: &'static [u8] = b"trader_state";
    pub fn space() -> usize {
        // CU Phase 1: zero-copy Pod account. AccountLoader requires the
        // allocated data length to EQUAL `8 (disc) + size_of::<Self>()`
        // EXACTLY — any "headroom" padding makes `load*()` fail with
        // bytemuck SizeMismatch. The struct is laid out in descending
        // alignment with an explicit `_pad` tail so size_of == 192 with
        // no implicit padding.
        8 + std::mem::size_of::<Self>()
    }

    /// Returns true if `signer` is authorized to act on this trader's
    /// behalf — either the trader themselves or a non-default delegate.
    pub fn is_authorized(&self, signer: &Pubkey) -> bool {
        signer == &self.trader || (self.delegate != Pubkey::default() && signer == &self.delegate)
    }
}

// MarketBondAccount removed in Flash Book V3: markets are now
// authority-gated only and there is no permissionless-deployer-bond
// infrastructure. Existing on-chain accounts from prior deployments are
// no longer touched by any instruction.

/// User-managed trading vault. A strategist deploys a vault and gets
/// trading authority over its collateral pool via the standard
/// TraderStateAccount.delegate mechanism. Depositors mint shares at the
/// current NAV; withdrawals burn shares for proportional NAV. The
/// strategist earns a high-water-mark performance fee, paid in newly
/// minted shares.
///
/// PDA seeds: [b"vault", strategist, vault_id]. vault_id is u8 → up to
/// 256 vaults per strategist. The vault's TraderStateAccount lives at
/// [b"trader_state", vault_pda] (i.e. the vault PDA is the "trader").
/// The strategist is set as `delegate` on that TraderState so they can
/// trade with their own keypair.
#[account]
#[derive(Debug)]
pub struct VaultAccount {
    pub strategist: Pubkey,
    pub bump: u8,
    pub vault_id: u8,
    /// 1 if accepting deposits, 0 if closed (withdrawals always allowed).
    pub accept_deposits: u8,
    /// Reserved padding for alignment.
    pub _pad0: u8,
    /// The vault PDA's TraderStateAccount (where collateral lives).
    pub trader_state: Pubkey,
    /// Display name (UTF-8, null-padded).
    pub name: [u8; 32],
    /// Performance fee in bps of NAV growth above the high-water mark.
    /// Charged on `settle_vault_perf_fee` by minting shares to the
    /// strategist. Typical: 1000–2000 (10–20%). Capped at BPS_DENOM/2.
    pub perf_fee_bps: u32,
    /// Total shares outstanding (sum of all VaultPositionAccount.shares).
    pub shares_outstanding: u64,
    /// Q64.0 NAV-per-share at the last performance crystallization.
    /// Stored × USD_UNIT for precision; 0 = bootstrap (perf fee not yet
    /// settled — first settle anchors the HWM at then-current NAV/share).
    pub hwm_nav_per_share_u64x6: u64,
    /// Unix timestamp of the last performance settlement.
    pub last_perf_settlement_unix: u64,
    /// Minimum deposit in quote-lots to prevent dust attacks. 0 = none.
    pub min_deposit_quote_lots: u64,
    /// Cumulative quote-lots deposited across the vault's lifetime
    /// (informational, used for ROI display).
    pub total_deposited_quote_lots: u64,
    /// Cumulative quote-lots withdrawn (gross, before perf fee).
    pub total_withdrawn_quote_lots: u64,
    /// Cumulative shares minted to the strategist as perf fee.
    pub total_perf_shares_minted: u64,
}

impl VaultAccount {
    pub const SEED: &'static [u8] = b"vault";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 1 + 1 + 1 + 32 + 32 + 4 + 8 + 8 + 8 + 8 + 8 + 8 + 8 = 168
        8 + 168
    }
}

/// Per-depositor share holding in a vault. Created lazily on first
/// deposit via init_if_needed.
///
/// PDA seeds: [b"vault_position", vault, depositor].
#[account]
#[derive(Debug)]
pub struct VaultPositionAccount {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub bump: u8,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
}

impl VaultPositionAccount {
    pub const SEED: &'static [u8] = b"vault_position";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 8 + 8 + 8 = 97; round to 112.
        8 + 112
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
    stp_mode: crate::matcher::order::StpMode::CancelNewest,
};
