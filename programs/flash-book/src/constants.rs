//! Compile-time constants. Anything tunable per-market lives in
//! `MarketParams` instead.

/// USD values use 6 decimals; `USD_UNIT = 10^USD_DECIMALS`. Token decimals are
/// *separate* and per-mint.
pub const USD_UNIT: u64 = 1_000_000;

/// Basis points denominator. 1 bp = 1/10_000.
pub const BPS_DENOM: u32 = 10_000;

/// Maximum fee discount allowed via `set_trader_fee_tier`. Capped at
/// 10_000 (100%): a discount can zero out the taker fee but can never make
/// it negative. A >100% discount would credit the taker collateral that is
/// never debited from any funding source — an unbacked mint — because the
/// fee waterfall floors at zero via `saturating_sub`. A negative taker fee
/// would only be sound if it debited a real source and reverted when
/// uncovered; no such path exists, so the cap is a hard 100%. The maker
/// rebate (`maker_rebate_bps`) is a separate, funded path and is unaffected.
pub const MAX_FEE_DISCOUNT_BPS: u32 = 10_000;

/// Hard cap on any single tier's `taker_fee_bps` or `maker_rebate_bps`.
/// 1_000 bps = 10% — above any plausible real fee schedule. Acts as a typo
/// guard at `init_fee_tiers` / `update_fee_tiers` write time; the authority
/// cannot accidentally lock traders into 90%+ fees.
pub const MAX_FEE_TIER_BPS: u32 = 1_000;

/// Default volume-window length used by `apply_fill` when no
/// `FeeTiersAccount` configuration is loaded (apply_fill stays a hot path;
/// it does not load the singleton FeeTiers PDA on every fill).
/// 14 days × 24h × 60m × 60s / 0.4 s/slot = 3_024_000 slots — a standard
/// rolling fee-tier window. The authority can override via
/// `FeeTiersAccount.volume_window_slots` for read paths
/// (`view_trader_effective_tier`).
pub const DEFAULT_VOLUME_WINDOW_SLOTS: u64 = 3_024_000;

/// Maximum positions per trader, used for stress-lattice loops.
pub const MAX_POSITIONS_PER_TRADER: usize = 16;

/// Max rungs a single `place_ladder_order` may place (4.3). Bounds the per-tx compute
/// so a ladder can never exceed the CU budget; each rung is a full `place_limit_v2_core`.
pub const MAX_LADDER_LEVELS: u8 = 20;

/// Maximum stress scenarios `assess_margin` accepts (enforced at entry).
/// `default_scenarios` emits exactly `5 + 8·N` scenarios for `N` markets
/// (1 flat + 8 single-market shocks per market + 4 uniform
/// all-up/down/black-swan), so with `N ≤ MAX_POSITIONS_PER_TRADER` the true
/// maximum is `5 + 8·16 = 133`. Setting the cap to exactly that bound keeps
/// the compute cost bounded (≤ 133 × 16 ≈ 2128 evaluations) without ever
/// rejecting a legitimate caller.
pub const MAX_STRESS_SCENARIOS: usize = 5 + 8 * MAX_POSITIONS_PER_TRADER;

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

/// Minimum slots an FLP liquidity provider must hold before withdrawing,
/// enforced by `withdraw_flp_capital` via `matcher::jit_lp_defense::can_withdraw`.
/// Defeats the flash / short-window attack of depositing right before a fee /
/// realized-PnL event that lifts FLP NAV and redeeming the windfall without
/// bearing risk. ~1 min at ~0.4s/slot — negligible for genuine LPs (who hold for days), fatal to the
/// timed windfall. Security floor; a future governance field can override it once
/// the FlpExposureAccount layout is versioned (it currently has no reserved space).
pub const FLP_MIN_HOLD_SLOTS: u64 = 150;

/// TTL (in slots) stamped on the resting order a REDUCE-ONLY trigger
/// (stop-loss / take-profit / bracket leg) injects when it fires. A fill is
/// a two-sided exchange committed to the FIFO ring, so settlement can never
/// reject or resize it (that would wedge the ring or break conservation) —
/// reduce-only is enforced at match time, and the injected close order is
/// additionally time-bounded: if the position closes between fire and fill,
/// an unexpiring order could rest indefinitely and later OPEN/FLIP a fresh
/// position against the trader's intent. The matcher skips expired resting
/// orders, so this TTL hard-caps that window; after it the order can never
/// fill and is reaped. ~5 min at ~0.4s/slot — generous for a genuine stop
/// to fill in any liquid market.
pub const REDUCE_ONLY_TRIGGER_ORDER_TTL_SLOTS: u64 = 750;

/// Delay (unix seconds) a proposed market-params change must wait before it
/// can be executed. 48h gives LPs and traders a window to see the
/// pre-announced change (the `ParamUpdateProposedEvent` carries the eta)
/// and exit or react before it lands. K-3: this timelocked path is the ONLY
/// way to change economic params — the immediate `update_market_params` is now
/// restricted to a single safety operation (enabling a disabled oracle-staleness
/// gate), so it can no longer change fees/margins/funding without notice.
pub const PARAM_UPDATE_TIMELOCK_SECONDS: i64 = 48 * 60 * 60;

/// K-3: sane bounds for the ONE change the immediate `update_market_params` path
/// still permits — ENABLING a disabled (legacy, pre-bound-era) oracle-staleness
/// gate (`oracle_staleness_max_seconds == 0`). The new bound must land in
/// `[MIN, MAX]`: the floor prevents an always-stale foot-gun (too-tight → every
/// price reads stale), the ceiling prevents "enabling" the gate to a uselessly
/// loose value. Any change to an ALREADY-enabled bound goes through the timelock.
pub const MIN_HEAL_STALENESS_SECONDS: u32 = 60;
pub const MAX_HEAL_STALENESS_SECONDS: u32 = 86_400;

/// G-3: sane bounds for a market's `oi_insurance_multiple_bps` when opting INTO
/// the OI-vs-insurance circuit breaker (0 = disabled is always allowed). The cap
/// is `insurance_balance · multiple_bps / BPS_DENOM`, so `multiple_bps` is how
/// many times the insurance balance the GROSS OI notional may reach. Floor 1×
/// (`10_000` bps) stops an operator self-DoS (a sub-1× cap would pause the market
/// on almost any OI); ceiling 10_000× bounds the loosest meaningful opt-in
/// (looser than that, just disable with 0).
pub const MIN_OI_INSURANCE_MULTIPLE_BPS: u64 = 10_000;
pub const MAX_OI_INSURANCE_MULTIPLE_BPS: u64 = 100_000_000;

/// K-2: minimum L1 slots between two `set_insurance_pause_threshold` changes.
/// The pause threshold is the ADL/insurance-pause trigger floor; without a
/// cooldown a compromised or erratic insurance authority could rapidly toggle
/// it to game exactly when ADL fires. ~1h at ~0.4s/slot — long enough to defeat
/// rapid toggling, short enough not to impede genuine governance. The first
/// update on a fresh fund (stamp == 0) is always allowed.
pub const INSURANCE_THRESHOLD_UPDATE_MIN_SLOTS: u64 = 9_000;

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

/// Max validators in a `SequencerCommittee`. 32 supports a healthy BFT set
/// (tolerates up to 10 Byzantine at N=31, f=10).
pub const MAX_COMMITTEE_VALIDATORS: usize = 32;

// Runtime bounds on the oracle-band mark clamp. The mark (an EMA of fills
// the semi-trusted sequencer produces) feeds worse-of(mark, oracle)
// liquidation health, so it MUST stay pinned near the trustless oracle.
// `MarketParams.oracle_band_bps` is the configured deviation, but a market
// could set it to 0 (no clamp — a manipulated mark could then drive
// wrongful liquidations) or absurdly wide. `apply_fill` enforces these
// bounds on the EFFECTIVE band regardless of config: an unset band (0)
// uses `DEFAULT`, and any stored band is capped to `MAX`, so a fresh
// oracle always pins the mark within `MAX` of the oracle.
pub const DEFAULT_ORACLE_BAND_BPS: u32 = 200; // 2% when the market left it unset
pub const MAX_ORACLE_BAND_BPS: u32 = 500; // 5% hard ceiling on the effective band

/// Runtime-effective oracle band (bps) used by `apply_fill`'s mark clamp:
/// `DEFAULT_ORACLE_BAND_BPS` when the market left it unset (0), otherwise the
/// stored band capped to `MAX_ORACLE_BAND_BPS`. Result is always in
/// `[1, MAX_ORACLE_BAND_BPS]`, so a fresh oracle always pins the mark. Pure so
/// the default/cap logic is unit-testable independent of the settlement path.
#[inline]
pub fn effective_oracle_band_bps(stored_band_bps: u32) -> u32 {
    if stored_band_bps == 0 {
        DEFAULT_ORACLE_BAND_BPS
    } else {
        stored_band_bps.min(MAX_ORACLE_BAND_BPS)
    }
}

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

/// Censorship BACKSTOP timeout. `FORCE_UNDELEGATE_TIMEOUT_SLOTS` governs
/// the FAST escape, which requires NO ER liveness signal at all (no fill
/// AND no `er_heartbeat`) — i.e. the ER is genuinely dark. But an
/// alive-but-CENSORING sequencer can keep heartbeating while including zero
/// trades, which would otherwise trap traders forever. So a second, much
/// longer threshold gates on SETTLEMENT liveness alone
/// (`last_mark_update_slot`), ignoring the heartbeat: if the market has
/// settled NOTHING for this long, the escape opens regardless of
/// heartbeats. ~0.4s/slot ⇒ 9000 slots ≈ 1 hour — long enough that a
/// healthy-but-quiet market is never griefed off the ER on a normal lull
/// (the heartbeat keeps the fast path shut), short enough that censorship
/// cannot trap funds indefinitely.
pub const CENSORSHIP_ESCAPE_TIMEOUT_SLOTS: u64 = 9_000;

/// Max reader allow-list size for a private (dark-pool) book's ephemeral
/// permission. Bounds the `set_book_privacy` CPI data (`8 + 1 + N*33` bytes) so a
/// single permission update stays well inside instruction-data limits. 32 readers
/// ⇒ ~1065 bytes.
pub const MAX_PRIVACY_MEMBERS: usize = 32;

/// Protocol-level safety cap on how far an `apply_flp_fill` price may
/// deviate from the FRESH oracle, in bps (symmetric band). The FLP quoter
/// always prices within its spread of fair value, so a legitimate fill is
/// far inside this bound; the cap exists to stop a compromised sequencer
/// settling an FLP fill far enough from the oracle to drain the pool. 3% is
/// comfortably above any realistic FLP spread (sub-2% even in stress) while
/// capping per-fill pool value-extraction at 3% of notional. A constant
/// (not a `MarketParams` field) because `MarketParams` has no reserved
/// slack; a governance override requires a versioned layout.
pub const FLP_MAX_FILL_DEVIATION_BPS: u32 = 300;

/// Rate limit for the PERMISSIONLESS `flp_refresh_quotes`: while the pool's quotes
/// are still resting, a keeper may only re-quote once they are at least this many
/// slots old, so nobody can churn the book. Consumed/stale quotes (none resting)
/// can always be re-posted immediately — so this only throttles re-quoting of
/// UNFILLED quotes, where freshness barely matters (the spread absorbs small
/// moves). 50 slots ≈ 20s at ~0.4s/slot: a real anti-churn floor (a tighter value
/// like 10 slots ≈ 4s is below normal tx-confirmation latency, so it throttles
/// almost nothing). A future governance field can tune it once MarketParams is
/// versioned.
pub const FLP_REFRESH_MIN_SLOTS: u32 = 50;

/// Anti-book-stuffing: max deviation (bps, symmetric) a RESTING order's
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

/// Anti-fragmentation price rule (roadmap 4.2): an order price may carry at most this
/// many significant figures. Prices with excess precision splinter the book into many
/// near-identical levels; capping significant figures keeps price-time priority meaningful
/// without constraining the market's tick size. 5 is a conventional exchange choice.
pub const MAX_PRICE_SIG_FIGS: u32 = 5;

/// Max expired orders a single `reap_expired_orders` call may reclaim.
/// Bounds CU/transaction size; the permissionless cranker batches more across
/// calls. 64 comfortably fits a transaction's account/data budget.
pub const MAX_REAP_PER_CALL: usize = 64;

// ─── HIP-3 permissionless-market safety envelope ────────────────────────────
// Hard bounds every `create_permissionless_market` param must satisfy, so a
// market created by ANY signer is provably conservative regardless of intent.
// (`validate_hip3_params` enforces these; `hip3_params_are_safe` proves it.)

/// Max leverage a permissionless market may offer. Conservative vs. the
/// authority path — a hostile creator can't lure with 100x then let it blow up.
pub const HIP3_MAX_LEVERAGE: u32 = 10;
/// Minimum maintenance-margin ratio (bps). 5% floor bounds the worst-case
/// gap between a breach and the liquidation fill.
pub const HIP3_MIN_MAINTENANCE_MARGIN_BPS: u32 = 500;
/// Max taker fee (bps). 1% cap — no predatory fee extraction.
pub const HIP3_MAX_TAKER_FEE_BPS: u32 = 100;
/// Max liquidation penalty + liquidator reward (bps). 10% cap.
pub const HIP3_MAX_LIQ_BPS: u32 = 1_000;
/// Max oracle staleness (seconds) — a permissionless market must consume a
/// FRESH oracle; 120 s bounds mark drift feeding worse-of liquidations.
pub const HIP3_MAX_ORACLE_STALENESS_SECS: u32 = 120;
/// Max any single fee-share (referrer/builder/creator), bps.
pub const HIP3_MAX_SHARE_BPS: u32 = 2_000;

#[cfg(test)]
mod oracle_band_tests {
    use super::*;

    #[test]
    fn unset_band_uses_tight_default() {
        assert_eq!(effective_oracle_band_bps(0), DEFAULT_ORACLE_BAND_BPS);
    }

    #[test]
    fn configured_band_within_ceiling_is_kept() {
        assert_eq!(effective_oracle_band_bps(100), 100);
        assert_eq!(
            effective_oracle_band_bps(MAX_ORACLE_BAND_BPS),
            MAX_ORACLE_BAND_BPS
        );
    }

    #[test]
    fn wide_band_is_capped_to_max() {
        assert_eq!(effective_oracle_band_bps(800), MAX_ORACLE_BAND_BPS);
        assert_eq!(effective_oracle_band_bps(u32::MAX), MAX_ORACLE_BAND_BPS);
    }

    #[test]
    fn effective_band_is_always_active_and_tight() {
        for stored in [0u32, 1, 100, 499, 500, 501, 10_000, u32::MAX] {
            let eff = effective_oracle_band_bps(stored);
            assert!(
                (1..=MAX_ORACLE_BAND_BPS).contains(&eff),
                "stored={stored} eff={eff}"
            );
        }
    }
}
