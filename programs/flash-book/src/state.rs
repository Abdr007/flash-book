//! On-chain account types. These are the persistent state that lives in
//! the ER (when delegated) or on Solana mainnet (when undelegated).
//!
//! All structs carry Anchor's 8-byte discriminator prefix. Hot accounts
//! (`PositionAccount`, `TraderStateAccount`) use zero-copy Pod layouts;
//! the rest use Borsh, which is sufficient under MAX_ORDERS_PER_BATCH.

use crate::constants::{
    MARK_HISTORY_LEN, MAX_FLP_QUOTE_LEVELS, MAX_ORDERS_PER_BATCH, MAX_POSITIONS_PER_TRADER,
};
use crate::matcher::funding::FundingIndex;
use crate::matcher::lot::{BaseLots, Bps, Ticks};
use crate::matcher::order::{Order, Side};
use crate::matcher::vpin::VpinState;
use anchor_lang::prelude::*;

/// Per-market parameters. Set at market initialization, updated only via
/// governance.
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize)]
pub struct MarketParams {
    pub tick_size: u64,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub min_base_lots: u64,

    pub taker_fee_bps: u32,
    /// Maker fee/rebate rate. SIGNED — positive = rebate paid to maker
    /// (MM incentive); negative = fee charged to maker
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
    pub flp_spread_delta_bps: u32, // realized-vol coefficient
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
    /// 4-8 slots (~2-4 seconds). 0 = no cooldown.
    pub liquidation_cooldown_slots: u32,

    /// Slots over which the liquidator reward grows from base to full.
    /// Dutch-style auction on the REWARD: first responders get a smaller
    /// reward, later responders progressively larger up to the full
    /// `liquidator_reward_bps`. 0 = reward is always full.
    /// Typical setting: 8-16 slots (~4-8 seconds). Encourages a
    /// competitive keeper pool to spread out instead of all racing the
    /// same block.
    pub liquidation_auction_duration_slots: u32,

    /// Drift-style JIT bonus: extra bps of rebate the maker earns when
    /// filling a JIT-tagged taker order (flag bit 3 on place_limit_order_v2).
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
    /// place_limit_order_v2 intake. Distinct from `max_position_lots_per_trader`
    /// (per-trader) and `max_position_ratio_bps` (per-trader as % of FLP);
    /// this caps the WHOLE-MARKET aggregate. Typical: scaled with FLP
    /// capital × leverage so worst-case insurance draw stays bounded.
    pub max_oi_base_lots: u64,

    /// Maximum allowed mark-price change per batch in bps. 0 = unlimited
    /// (pre-launch markets often run open). When set, the
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
    /// 0 = disabled (single-tick premium). When > 0, the funding
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
    /// 0 = disabled.
    pub funding_per_period_max_bps: u32,
    /// Period length for the funding cap, in seconds. Typical: 86_400
    /// (24h). Ignored if `funding_per_period_max_bps == 0`.
    pub funding_period_seconds: u32,

    /// Bootstrap-period batches for permissionless markets. Within the
    /// first N batches after a market is initialized, all per-trader
    /// and whole-market position/OI caps are tightened by a factor of
    /// 4 to defend against snipers in the price-discovery window. After
    /// `current_batch >= bootstrap_period_batches`, normal caps apply.
    /// 0 = disabled (protocol-curated deploys).
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

    /// Anti-dust minimum order notional in quote-lots (4.1): an order's
    /// `size_lots × price_ticks × tick_size` must be ≥ this. Tiny orders pass the
    /// per-lot floor (`min_base_lots`) yet carry negligible value, letting a spammer
    /// splinter the book cheaply; this puts a real value floor on every resting order.
    /// `0` = disabled (no per-order value floor).
    pub min_notional_quote_lots: u64,
}

/// Top-level market state. One per pool market (e.g. SOL/USD, BTC/USD).
#[account]
#[derive(Debug)]
pub struct MarketAccount {
    pub authority: Pubkey,
    /// Curated-creator pubkey. Always zeroed by `initialize_market`
    /// (markets are authority-gated — no permissionless creator share is
    /// paid out). When non-default, every fill on this market emits a
    /// CreatorFeeOwedEvent crediting the creator with
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
    /// DORMANT: funding is plumbed but unwired. No instruction ever sets
    /// `last_funding_rate_bps_per_sec` nonzero (the rate formula lives only in
    /// the read-only `view_predicted_funding`; nothing stores its result), so
    /// `settle_funding` accrues and charges exactly 0 and `cum_funding_index`
    /// never advances. The funding-nets-to-zero invariant holds trivially.
    /// Both fields are account layout and `settle_funding` is a live,
    /// reachable instruction; activating funding means computing and storing
    /// the rate, with its own conservation proof and devnet cycle.
    pub cum_funding_index: i128,
    pub last_funding_rate_bps_per_sec: i64,
    /// Layout-reserved: the VPIN accumulator is retired (see
    /// `matcher/vpin.rs`). No instruction advances it, every market carries
    /// the zero value, and `total_toxicity_tax_collected` never grows.
    /// Removing an on-chain field is a state migration, so the bytes remain.
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
    /// Deliberately decoupled from `authority`: the authority can be burned
    /// (zeroed) for decentralization, but settlement must keep working — so
    /// the writer that can post fills is gated by this dedicated,
    /// separately-rotatable key instead.
    ///
    /// Set to `authority` at init; rotate via `set_market_sequencer`
    /// (authority-gated, so rotate BEFORE burning authority). Trailing
    /// field: accounts serialized before it existed deserialize it as the
    /// zero pubkey — which is unsignable, so `apply_fill` fails closed
    /// (refuses every fill) until the authority sets a sequencer.
    pub sequencer: Pubkey,
    /// Monotonic settlement nonce. Every `apply_fill` / `apply_flp_fill`
    /// must carry a `fill_seq` STRICTLY GREATER than this, after which it is
    /// stored here — so a replayed or out-of-order settlement (a crashed /
    /// restarting sequencer re-emitting an already-applied batch, or a
    /// compromised key resubmitting one) is rejected on-chain. Trailing
    /// field within `space()` headroom: pre-existing accounts deserialize it
    /// as 0, so the first real fill (`fill_seq` ≥ 1) passes.
    pub last_settlement_seq: u64,
    /// Sticky flag: once `init_fill_commitment` arms this market, the
    /// fill-commitment ring is MANDATORY in `apply_fill` — a (compromised)
    /// sequencer cannot bypass the anti-fabrication guard by omitting the
    /// optional ring account. Never cleared. Trailing field: accounts
    /// serialized before it existed deserialize it as `false`.
    pub fill_commitment_required: bool,
    /// Sticky flag: once `initialize_haircut_state` enables the haircut
    /// junior-claim engine for this market, the (optional) haircut accounts
    /// are MANDATORY in `apply_fill`/`apply_flp_fill`. Without this a
    /// settlement could omit them and route positive realized PnL straight to
    /// collateral with no Residual/solvency gating (then withdraw it).
    /// Trailing field: pre-existing accounts deserialize it as `false`.
    pub haircut_enabled: bool,
    /// L1 slot at which the mark price was last actively maintained — written by
    /// BOTH the fill-EMA path in `apply_fill` (every fill = ER alive) and by
    /// `settle_mark`. Distinct from `last_mark_settle_slot` (settle-only): this
    /// tracks mark FRESHNESS across both update paths so a stalled ER is
    /// detectable. When `current_slot - last_mark_update_slot` exceeds
    /// `constants::MARK_STALENESS_MAX_SLOTS`, liquidation falls back to
    /// oracle-only pricing and the market auto-pauses. Trailing field:
    /// accounts serialized before it existed deserialize it as 0 — treated as
    /// "freshness unknown ⇒ liquidate oracle-only" (fail-safe) until the
    /// first fill/settle stamps it.
    pub last_mark_update_slot: u64,
    /// L1 slot at which the market book was last delegated to the ER (set by
    /// `delegate_market_book`). Used as the settlement-liveness BASELINE for the
    /// permissionless `force_undelegate_market_book` escape, so the censorship
    /// timeout starts ticking from delegation even if the sequencer NEVER posts a
    /// fill (which would otherwise keep `last_mark_update_slot == 0` and trap
    /// pre-existing positions forever). The gate uses
    /// `max(last_mark_update_slot, book_delegated_at_slot)`. Trailing field:
    /// pre-existing accounts deserialize it as 0.
    pub book_delegated_at_slot: u64,
    /// ER liveness HEARTBEAT slot, stamped by the sequencer-authenticated
    /// `er_heartbeat` ix independent of trade flow. Fill/settle-driven
    /// signals (`last_mark_update_slot`) advance only on a fill or
    /// `settle_mark`, so without a heartbeat a healthy-but-QUIET market is
    /// indistinguishable from a stalled one and would auto-pause / force-
    /// undelegate. The heartbeat lets the chain distinguish "ER alive, no
    /// trades" (heartbeat fresh) from "ER dead" (heartbeat stale).
    /// Auto-pause and the FAST force-undelegate path use
    /// `max(last_mark_update_slot, last_heartbeat_slot)`; the censorship
    /// backstop deliberately ignores it (an alive-but-censoring sequencer
    /// heartbeats). Must be sequencer-authenticated — a permissionless
    /// heartbeat would let anyone keep a dead market "alive" and block the
    /// escape. Trailing field: pre-existing accounts deserialize it as 0.
    pub last_heartbeat_slot: u64,
    /// Per-market matcher batch cap — the max resting levels a taker may
    /// cross in one `place_taker_order_v2` tx. Trailing field
    /// (`size_of::<MarketAccount>()` is 896 B with 256 B free under
    /// `space() = 1152`; pre-existing accounts deserialize it as `0`). `0`
    /// means the global `MAX_BATCH_ORDERS_PER_SIDE_V2` (96) — the log-safe
    /// default. Raised (≤ `FILL_RING_CAP` = 256) ONLY by `init_fill_outbox`,
    /// which simultaneously arms the on-chain fill-outbox so the crossed
    /// fills are delivered OFF the program log: a cap above the ~96 log-safe
    /// point without an outbox would truncate fills in the 10 KB log and
    /// wedge settlement.
    pub max_batch_orders: u16,

    /// Total base-lot volume of fills that have been MATCHED (pushed to the
    /// fill-commitment ring) but NOT yet settled by apply_fill /
    /// apply_flp_fill. Settled OI (`oi_long/short_lots`) only advances at
    /// settlement, so without this reserve the per-market OI cap could be
    /// overshot by pipelining takers ahead of settlement; the intake cap
    /// checks `oi[side] + unsettled_fill_volume + new_size`. Incremented per
    /// fill at match (produce), decremented per fill at settle — tied 1:1 to
    /// the ring's FIFO produce/settle lifecycle, so it self-balances (cancels
    /// produce no fills). Trailing field ⇒ existing accounts read it as 0.
    pub unsettled_fill_volume: u64,

    /// True while the book is delegated to the ER. Set by `delegate_market_book`,
    /// cleared by `clear_book_delegation` when the book is back on L1. Order
    /// placement requires the trader's `er_margin_ready` when this is true, so
    /// every ER order belongs to a trader whose reserved margin the sequencer
    /// can attest. Fail closed: a stale `true` only over-requires the
    /// attestation account (safe). Trailing field ⇒ existing accounts read it
    /// as `false`.
    pub book_delegated: bool,

    /// Slot of the last REAL settlement (a committed fill applied via
    /// `apply_fill` / `apply_flp_fill`). Unlike `last_mark_update_slot` — which
    /// the permissionless `settle_mark` also bumps — this advances ONLY on
    /// genuine settlement, so it is the honest liveness signal for the
    /// force-undelegate escape and the S7 auto-pause: a censoring sequencer that
    /// heartbeats and spams `settle_mark` but settles no fills cannot keep this
    /// fresh. Trailing field ⇒ existing accounts read it as 0 (never settled),
    /// which the escape treats via the `book_delegated_at_slot` baseline exactly
    /// as a never-stamped mark was.
    pub last_settlement_slot: u64,
    /// Unix time of the last funding-index advance by the funding crank. The
    /// crank accrues `rate · (now − last_funding_crank_unix)` into
    /// `cum_funding_index` and then restamps this. Trailing field ⇒ pre-existing
    /// accounts read it as 0; the crank treats 0 as "never cranked" and only
    /// seeds the timestamp (no accrual on the first tick), so it can never apply
    /// a rate over an unbounded Δt from an uninitialised clock.
    pub last_funding_crank_unix: u64,
}

/// Optional emergency guardian for one market, held in a SEPARATE PDA (not a
/// MarketAccount field — adding 32 B there pushes several `try_accounts`
/// frames past the 4 KB BPF stack limit). A guardian may only
/// RESTRICT market status (→ PostOnly/Paused/Closed, monotonic — the fast, fail-safe
/// direction), NEVER loosen (→ Active/Inactive stays authority-only), so a
/// compromised guardian can pause/close but never re-open. Absence of this account
/// (or `guardian == Pubkey::default()`) = no guardian. Set/cleared by the market
/// authority via `set_guardian`; read (optionally) by `set_market_status`.
#[account]
pub struct MarketGuardianAccount {
    pub market: Pubkey,
    pub guardian: Pubkey,
    pub bump: u8,
}

impl MarketGuardianAccount {
    pub const SEED: &'static [u8] = b"market_guardian";
    pub const LEN: usize = 32 + 32 + 1;
}

/// Pending authority for a 2-step (propose→accept) market-authority
/// transfer. Held in its own PDA (not a MarketAccount field — the same
/// stack constraint as the guardian). `propose_authority_transfer` (current authority)
/// stores `pending_authority`; `accept_authority_transfer`, signed BY that pending
/// key, commits `market.authority` and closes this account. Because the new key must
/// itself sign to accept, a transfer can never strand control at a wrong/dead key
/// (the failure mode the 1-step `transfer_market_authority` permits).
#[account]
pub struct MarketPendingAuthorityAccount {
    pub market: Pubkey,
    pub pending_authority: Pubkey,
    /// The authority that PROPOSED this transfer. `accept_authority_transfer`
    /// requires it to still equal `market.authority`, so a pending proposed by
    /// an authority that has since been replaced (via the 1-step
    /// `transfer_market_authority` or a re-propose) can never displace the
    /// current authority.
    pub proposed_by: Pubkey,
    pub bump: u8,
}

impl MarketPendingAuthorityAccount {
    pub const SEED: &'static [u8] = b"pending_authority";
    pub const LEN: usize = 32 + 32 + 32 + 1;
}

/// A timelocked market-params update. Own PDA (not a MarketAccount field —
/// the same stack constraint). `propose_param_update` validates the
/// new params, stores `keccak(params)` + an `eta_unix` (= now + timelock), and does
/// NOT apply. `execute_param_update` applies the params only once `now >= eta` AND
/// the supplied params hash to the stored `params_hash` — so the executed change is
/// exactly the pre-announced one, after the delay. `cancel_param_update` revokes it.
#[account]
pub struct PendingParamUpdateAccount {
    pub market: Pubkey,
    pub params_hash: [u8; 32],
    pub eta_unix: i64,
    pub bump: u8,
}

impl PendingParamUpdateAccount {
    pub const SEED: &'static [u8] = b"pending_params";
    pub const LEN: usize = 32 + 32 + 8 + 1;
}

// Build-time guard — the struct must fit the allocated `space()` (8 disc +
// 1152). If a future field overflows it, this fails the build instead of
// silently corrupting account (de)serialization.
const _: () = assert!(
    ::core::mem::size_of::<MarketAccount>() <= 1152,
    "MarketAccount exceeds its allocated space() — bump space() before adding fields"
);

impl MarketAccount {
    pub const SEED: &'static [u8] = b"market";
    /// Effective matcher batch cap. `max_batch_orders` if a market has opted into
    /// a raised cap (always paired with an armed fill-outbox), else the global
    /// log-safe default `MAX_BATCH_ORDERS_PER_SIDE_V2`. `0` (unset) ⇒
    /// default. Clamped to `FILL_RING_CAP` so a corrupt field can never exceed the
    /// commitment-ring / outbox capacity.
    pub fn effective_batch_cap(&self) -> usize {
        let cap = if self.max_batch_orders == 0 {
            crate::MAX_BATCH_ORDERS_PER_SIDE_V2
        } else {
            self.max_batch_orders as usize
        };
        cap.min(crate::matcher::fill_commitment::FILL_RING_CAP as usize)
    }
    pub fn space() -> usize {
        // 8 (anchor disc) + a pinned upper bound with headroom over
        // `size_of::<MarketAccount>()` (build-guarded above at ≤ 1152).
        8 + 1152
    }
}

/// Multi-tier fee table. Global per-program, authority-set. The standard
/// volume-tier model:
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
/// Coexists with `fee_discount_bps`: tier's bps SUPERSEDE
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
    /// SIGNED maker rate. Positive = rebate paid TO maker
    /// (MM-incentive semantics); negative = fee charged FROM maker
    /// (low-tier retail). Validated to be monotone non-decreasing
    /// across tiers (a higher-volume trader's maker treatment is
    /// never worse than a lower-volume trader's).
    pub maker_rebate_bps: i32,
    pub taker_fee_bps: u32,
}

/// Major venues run 7–9 volume tiers; 10 covers both with headroom.
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
/// ─── ISOLATED-MARGIN MARKER ──────────────────────────────────────────
/// `collateral_quote_lots` encodes the position's margin mode:
///
///   collateral_quote_lots == 0  → cross margin (default). The position
///                                 is backed by the trader's pooled
///                                 `TraderStateAccount.collateral_quote_lots`.
///
///   collateral_quote_lots  > 0  → isolated margin. The position is
///                                 backed ONLY by this amount; the
///                                 trader's pooled collateral is
///                                 insulated from this position's
///                                 liquidation.
///
/// The marker is enforced end-to-end:
///
///   1. `assess_margin_split` (matcher/risk.rs) splits the trader's
///      position set into cross and isolated, evaluates each isolated
///      position against its own collateral, and only includes cross
///      positions in the pooled-collateral assessment.
///   2. `liquidate_position_v2` draws the penalty + liquidator reward
///      from `position.collateral_quote_lots` first for an isolated
///      position, with the insurance fund covering any shortfall — the
///      trader's main pool is never touched.
///   3. `set_position_isolated(amount)` / `set_position_cross()` move
///      collateral between `TraderState.collateral_quote_lots` and
///      `PositionAccount.collateral_quote_lots`, gated by a
///      post-transfer health check on BOTH the cross set and the
///      isolated position.
#[account(zero_copy)]
#[derive(Debug)]
pub struct PositionAccount {
    // Zero-copy Pod layout. The i128 is placed FIRST at a
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
    /// Slot of the most recent deposit. (Re)set on every deposit via
    /// `jit_lp_defense::extend_lock_on_deposit`; `withdraw_flp_capital` is gated
    /// on `jit_lp_defense::can_withdraw(deposited_at_slot, now, FLP_MIN_HOLD_SLOTS)`
    /// to defeat flash / short-window deposit→NAV-windfall→redeem. Trailing
    /// field within the allocation slack: pre-existing LP accounts
    /// deserialize it as 0 ⇒ immediately withdrawable.
    pub deposited_at_slot: u64,
}

impl LpPositionAccount {
    pub const SEED: &'static [u8] = b"lp_position";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 8 + 8 + 8 + 8 (deposited_at_slot) = 73; the
        // `8 + 96` allocation covers it with 31 bytes of slack.
        8 + 96
    }
}

/// Protocol-wide FLP-system lock. The singleton `FlpExposureAccount`
/// (`apply_flp_fill` books every FLP fill into it) and the per-market v3 FLP
/// redeem from the SAME vault, so LP shares outstanding in both would
/// double-count the same realized PnL and let the last redeemers over-withdraw.
/// This singleton records which system minted shares first; the other system's
/// share-minting deposit then fails closed. A pure lock — it holds no funds and
/// no per-market state, so it is a NEW account with its own reserved slack (no
/// existing layout is resized).
#[account]
#[derive(Debug)]
pub struct FlpModeAccount {
    pub bump: u8,
    /// 0 = unset, 1 = singleton, 2 = per-market v3.
    pub mode: u8,
    pub _reserved: [u8; 6],
}
impl FlpModeAccount {
    pub const SEED: &'static [u8] = b"flp_mode";
    pub const MODE_UNSET: u8 = 0;
    pub const MODE_SINGLETON: u8 = 1;
    pub const MODE_V3: u8 = 2;
    pub fn space() -> usize {
        8 + 8
    }
}

/// Per-trader state. Holds collateral, last-settled funding marker, and
/// position-list pointers (Position PDAs are separate accounts; this is
/// a lightweight index).
#[account(zero_copy)]
#[derive(Debug)]
pub struct TraderStateAccount {
    // ── Zero-copy Pod layout ─────────────────────────────────────────────
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
    /// Rolling notional in the current volume window (quote lots,
    /// maker + taker). Reset on window expiry; drives `resolve_fee_tier`.
    pub volume_30d_quote_lots: u64,
    /// Slot the current volume window opened.
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
    /// Sub-account index. `0` = main; `1..=255` = sub. Set at
    /// `open_trader_state` (0) / `open_trader_sub_account` (sub_index).
    pub sub_index: u8,
    /// Cross-domain (ER) margin enforcement flag. `0` = not ER-active: the
    /// strict L1 withdraw paths apply (and any account whose byte here is
    /// still zero padding reads as not ER-active). `1` = the trader has
    /// ER-reserved margin attested, so collateral withdrawals MUST route
    /// through the cross-domain variants (which honor `ErMarginAttestation`);
    /// the strict paths fail closed with `UseXDomainWithdraw`. Occupies a
    /// byte of the `_pad` tail, so the Pod layout stays 192 bytes.
    pub er_active: u8,
    /// ER-readiness flag. `1` = this trader's `ErMarginAttestation` account
    /// exists (set once by `init_er_margin_attestation`, never cleared), so the
    /// sequencer can always attest the margin reserved by the trader's live ER
    /// orders. Order placement on a delegated book requires it; withdrawals
    /// never consult it — the reserved-margin gate (`er_active` +
    /// `ErMarginAttestation`) is what protects collateral backing ER orders.
    /// Carved from the `_pad` tail: accounts written before this field existed
    /// read it as `0` (not ready), so every pre-existing trader is unchanged and
    /// the Pod layout stays 192 bytes.
    pub er_margin_ready: u8,
    pub _pad: [u8; 3],
}

impl TraderStateAccount {
    pub const SEED: &'static [u8] = b"trader_state";
    pub fn space() -> usize {
        // Zero-copy Pod account. AccountLoader requires the
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

// Markets are authority-gated only; there is no permissionless-deployer
// bond account type in this program.

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
