//! Compile-time constants. Anything tunable per-market lives in
//! `MarketParams` instead.

/// USD values use 6 decimals throughout — matches Flash V2's existing
/// convention. Token decimals are *separate* and per-mint.
pub const USD_DECIMALS: u8 = 6;
pub const USD_UNIT: u64 = 1_000_000;

/// Basis points denominator. 1 bp = 1/10_000.
pub const BPS_DENOM: u32 = 10_000;

/// Maximum fee discount allowed via `set_trader_fee_tier`. Capped at
/// 10_000 (100%) — a discount zeroes out the taker fee but can never make
/// it negative.
///
/// M-5 (audit 2026-06) FIX: was 12_000 (120%), which enabled a NEGATIVE
/// top-tier fee (taker *paid* for flow). The doc claimed the rebate was
/// "sourced from the protocol's insurance contribution", but the apply_fill
/// path computed it with a `saturating_sub` that floors at zero — so the
/// >100% rebate credited the taker collateral that was NEVER debited from
/// insurance/Residual: an unbacked mint. Capping at 100% removes the
/// negative-fee tier (and the mint) entirely; the maker rebate
/// (`maker_rebate_bps`) is a separate, properly-funded path and is
/// unaffected. If negative taker fees are wanted later, they must debit a
/// real source and revert if uncovered (the deferred option B).
pub const MAX_FEE_DISCOUNT_BPS: u32 = 10_000;

/// WAVE 22: hard cap on any single tier's `taker_fee_bps` or
/// `maker_rebate_bps`. 1_000 bps = 10% — well above HL's worst-tier
/// taker fee (0.05%) and below any plausible "real" fee schedule. Acts
/// as a typo guard at `init_fee_tiers / update_fee_tiers` write time;
/// authority can't accidentally lock traders into 90%+ fees.
pub const MAX_FEE_TIER_BPS: u32 = 1_000;

/// WAVE 22: default volume-window length used by `apply_fill` when no
/// `FeeTiersAccount` configuration is loaded (apply_fill stays a hot
/// path; we don't make it load the singleton FeeTiers PDA on every
/// fill). 14 days × 24h × 60m × 60s / 0.4 s/slot = 3_024_000 slots —
/// matches HL's standard rolling window. Authority can override via
/// `FeeTiersAccount.volume_window_slots` for read paths
/// (`view_trader_effective_tier` + future matcher-integrated fee
/// resolution).
pub const DEFAULT_VOLUME_WINDOW_SLOTS: u64 = 3_024_000;

// HIP-3 deployer bond unbonding delay was removed alongside the
// permissionless market creation / bond infrastructure in Flash Book
// V3. Markets are now authority-gated only.

/// Maximum stress scenarios per batch — capped to keep margin compute bounded.
/// At 60 scenarios × 8 markets × 16 positions = 7680 evaluations per batch.
pub const MAX_STRESS_SCENARIOS: usize = 64;

/// Maximum positions per trader, used for stress-lattice loops.
pub const MAX_POSITIONS_PER_TRADER: usize = 16;

/// Maximum FLP quote levels per side per batch.
pub const MAX_FLP_QUOTE_LEVELS: usize = 16;

/// Maximum orders processed per batch (compute-budget bounded).
pub const MAX_ORDERS_PER_BATCH: usize = 256;


/// Maximum recent clearing prices retained for TWAP / volatility.
pub const MARK_HISTORY_LEN: usize = 16;

/// Cumulative funding index uses fixed-point Q64.64 — enough for
/// 100+ years of accumulation at any reasonable rate without overflow.
pub const FUNDING_INDEX_FRACTIONAL_BITS: u32 = 64;

/// VPIN EMA uses fixed-point Q32.32.
pub const VPIN_FRACTIONAL_BITS: u32 = 32;
pub const VPIN_FIXED_ONE: u64 = 1u64 << VPIN_FRACTIONAL_BITS;

/// Lot epsilon — sizes below this are treated as zero (rounding noise).
pub const LOT_EPSILON: u64 = 1;

/// Reserved sequence-number range for synthesized FLP virtual orders.
/// User-submitted orders use [0, FLP_SEQ_RESERVED_OFFSET); FLP virtual
/// quotes use [FLP_SEQ_RESERVED_OFFSET, ∞). Keeps user FIFO ordering
/// untouched by FLP injection.
pub const FLP_SEQ_RESERVED_OFFSET: u64 = 1u64 << 56;

/// Per-trader per-batch limit on submitted orders. Spam-protection.
pub const MAX_ORDERS_PER_TRADER_PER_BATCH: u32 = 16;

/// Solana account max size (10 MB). `migrate_market_to_v3` (and any
/// other account-reallocing ix) MUST refuse `target_size` greater than
/// this — otherwise the realloc panics deep in the runtime and burns
/// the tx's compute budget without surfacing a useful error.
pub const SOLANA_MAX_ACCOUNT_SIZE: usize = 10 * 1024 * 1024;

/// Hard cap on legs in a single `place_basket_order_n` call. Bounded
/// because remaining_accounts traversal is linear in legs and each leg
/// costs ~3 account deserialisations + a buffer re-serialise. Production
/// CLOBs typically size baskets at ≤4 legs (a long-short pair plus a
/// hedge); larger baskets land via repeated calls.
pub const MAX_BASKET_LEGS_N: usize = 4;

/// H8: minimum slots an FLP liquidity provider must hold before withdrawing,
/// enforced by `withdraw_flp_capital` via `matcher::jit_lp_defense::can_withdraw`.
/// Defeats the flash / short-window attack of depositing right before a fee /
/// realized-PnL event that lifts FLP NAV and redeeming the windfall without
/// bearing risk (the `jit_lp_defense` module existed but was never wired). ~1 min
/// at ~0.4s/slot — negligible for genuine LPs (who hold for days), fatal to the
/// timed windfall. Security floor; a future governance field can override it once
/// the FlpExposureAccount layout is versioned (it currently has no reserved space).
pub const FLP_MIN_HOLD_SLOTS: u64 = 150;

/// ER-stall safety floor: max L1 slots since the mark price last moved (via the
/// fill-EMA in `apply_fill` or a hard `settle_mark`) before the mark is treated
/// as STALE. A stalled MagicBlock ER freezes the fill stream, so the mark stops
/// updating; past this bound `liquidate_position_v2` drops the (possibly
/// adverse, frozen) mark and falls back to ORACLE-ONLY health pricing, and
/// `verify_market_invariants` auto-pauses the market so no new orders land while
/// the ER is down. ~0.4s/slot ⇒ 150 slots ≈ 60s — comfortably above the normal
/// fill/`settle_mark` cadence, so healthy markets never trip it. Security floor;
/// a future governance field can override it once MarketParams is versioned.
pub const MARK_STALENESS_MAX_SLOTS: u64 = 150;

/// Censorship / ER-stall escape threshold: L1 slots the ER may be silent (no
/// committed fill advancing `MarketAccount.last_mark_update_slot`) before ANY
/// caller may permissionlessly undelegate the market book / market back to L1
/// via `force_undelegate_market_book` / `force_undelegate_market` — freeing
/// trapped traders to close and withdraw WITHOUT the sequencer's cooperation.
/// Ties the exit guarantee to settlement liveness, not sequencer goodwill.
/// ~0.4s/slot ⇒ 750 slots ≈ 5 min, far beyond any healthy commit cadence, so it
/// never fires in normal operation — only on a genuinely dark / censoring ER.
/// Deliberately >> MARK_STALENESS_MAX_SLOTS (which only changes liquidation
/// PRICING): auto-pause/oracle-fallback engage first; forced exit is last resort.
pub const FORCE_UNDELEGATE_TIMEOUT_SLOTS: u64 = 750;

/// F3 (audit 2026-06) — censorship BACKSTOP timeout. `FORCE_UNDELEGATE_TIMEOUT_
/// SLOTS` now governs the FAST escape, which requires NO ER liveness signal at
/// all (no fill AND no `er_heartbeat`) — i.e. the ER is genuinely dark. But an
/// alive-but-CENSORING sequencer can keep heartbeating while including zero
/// trades, which would otherwise trap traders forever (defeating F1). So a
/// second, much longer threshold gates on SETTLEMENT liveness alone
/// (`last_mark_update_slot`), ignoring the heartbeat: if the market has settled
/// NOTHING for this long, the escape opens regardless of heartbeats. ~0.4s/slot
/// ⇒ 9000 slots ≈ 1 hour. Long enough that a healthy-but-quiet market is never
/// griefed off the ER on a normal lull (the heartbeat keeps the fast path shut),
/// short enough that censorship cannot trap funds indefinitely.
pub const CENSORSHIP_ESCAPE_TIMEOUT_SLOTS: u64 = 9_000;

/// #35 / H1 part B — protocol-level safety cap on how far an `apply_flp_fill`
/// price may deviate from the FRESH oracle, in bps (symmetric band). The FLP
/// quoter always prices within its spread of fair value, so a legitimate fill is
/// far inside this bound; the cap exists only to stop a compromised sequencer
/// settling an FLP fill far enough from the oracle to drain the pool.
/// M-1 (audit 2026-06) FIX: tightened 2000 bps (20%) → 300 bps (3%). 20% left a
/// compromised sequencer able to extract up to a fifth of notional per fill,
/// repeatable; 3% is still comfortably above any realistic FLP spread (sub-2%
/// even in stress) while capping per-fill pool value-extraction at 3% of
/// notional. A constant (not a `MarketParams` field) because `MarketParams` has
/// no reserved slack; a future governance override can replace it once that
/// layout is versioned.
pub const FLP_MAX_FILL_DEVIATION_BPS: u32 = 300;

/// #36 — anti-book-stuffing: max deviation (bps, symmetric) a RESTING order's
/// price may sit from the fresh oracle. Far-from-market orders are the classic
/// node-arena-exhaustion vector (cheap because they never fill); requiring a
/// resting order to be within band forces it close enough to market to bear real
/// fill/position risk, turning "free" stuffing into risky stuffing. 5000 bps =
/// 50% is generous — it rejects only absurd prices (a bid below half the oracle
/// or an ask above 1.5×), never a realistic limit (DCA / catch-a-dip orders sit
/// well inside). Enforced only when a live oracle anchors it (`oracle == 0`
/// skips). Reuses the Kani-proven `price_within_band` predicate. NOTE: this
/// raises the attacker's cost; it does not by itself stop a sybil (N wallets ×
/// orders) — that is bounded by the expandable arena + economic future work.
pub const MAX_RESTING_ORDER_DEVIATION_BPS: u32 = 5000;

/// #36 — max expired orders a single `reap_expired_orders` call may reclaim.
/// Bounds CU/transaction size; the permissionless cranker batches more across
/// calls. 64 comfortably fits a transaction's account/data budget.
pub const MAX_REAP_PER_CALL: usize = 64;
