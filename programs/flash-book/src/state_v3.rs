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
    /// Phase 2f — TraderState sub-account index this trigger fires
    /// against. Pre-Phase-2f accounts read back as 0 (main) from the
    /// trailing zeros of their allocated `space()`. ExecuteTriggerOrderV3
    /// copies this into the synthetic RestingOrderV2.sub_index so the
    /// resulting fill is routed to the right TraderState.
    pub sub_index: u8,
    /// Wave 27a — slippage cap on trigger execution (GMX V2 acceptablePrice
    /// pattern). When > 0, the trigger refuses to fire if the oracle has
    /// moved past `acceptable_price_ticks` in the direction unfavorable
    /// to the resulting order. Pre-Wave-27a triggers read this back as 0
    /// (from the trailing zeros of `space()`), meaning "no cap, legacy
    /// behavior" — full backward compatibility.
    ///
    /// Direction rules:
    /// - side == 0 (long, buying): refuse if `oracle > acceptable_price`.
    ///   Trader wanted to buy at trigger near `limit_price` but oracle
    ///   gapped up too far — the fill would be much worse than intended.
    /// - side == 1 (short, selling): refuse if `oracle < acceptable_price`.
    ///   Symmetric: stop-loss long that wanted to exit near trigger but
    ///   oracle gapped down too far.
    ///
    /// On refusal, the trigger DEACTIVATES (so it doesn't re-fire next
    /// slot at the same gapped price) and emits
    /// `TriggerOrderV3SlippageCancelledEvent`. The trader can re-place
    /// with updated params if they still want to act.
    pub acceptable_price_ticks: u64,
    /// AUDIT HIGH-7 (2026-07) — OCO link. For a bracket leg, this holds the
    /// sibling leg's `trigger_id`; when this leg FIRES, the executor must pass
    /// the sibling `TriggerOrderAccountV3` and it is deactivated, so both legs
    /// of a bracket can never both fire (which previously let a whipsaw
    /// double-fill flip the position). Set iff `flags & FLAG_HAS_SIBLING`.
    /// Pre-existing accounts read this as 0 (no sibling) from the trailing
    /// zeros of `space()`, and `FLAG_HAS_SIBLING` is unset — full backward
    /// compatibility (non-bracket triggers are unaffected).
    pub sibling_trigger_id: u8,
    /// Reserved for future expansion. Pre-existing accounts read this as
    /// zero from the trailing space().
    pub _reserved: [u8; 7],
}

impl TriggerOrderAccountV3 {
    pub const SEED: &'static [u8] = b"trigger_v3";
    pub const FLAG_REDUCE_ONLY: u8 = 1 << 0;
    pub const FLAG_ACTIVE: u8 = 1 << 1;
    /// AUDIT HIGH-7 (2026-07) — set on both legs of a bracket. Marks that
    /// `sibling_trigger_id` is valid and that firing this leg must deactivate
    /// the sibling (OCO). Unset on standalone triggers (backward compatible).
    pub const FLAG_HAS_SIBLING: u8 = 1 << 2;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+1 + 8+8+8+8+8 + 1 (sub_index) = 118.
        // Wave 27a: + 8 (acceptable_price) + 8 (reserved) = 134.
        // 8 + 128 = 136 still fits with 2 bytes spare.
        8 + 128
    }

    /// AUDIT HIGH-7 (2026-07) — true iff `other` is THIS trigger's genuine OCO
    /// sibling: same trader + market, `other.trigger_id` equals our
    /// `sibling_trigger_id`, AND `other` points back at us
    /// (`other.sibling_trigger_id == self.trigger_id`). The mutual back-link is
    /// what prevents a malicious executor from passing an unrelated trigger they
    /// own just to deactivate it. Pure function for unit-testability.
    pub fn is_oco_sibling(&self, other: &TriggerOrderAccountV3) -> bool {
        self.trader == other.trader
            && self.market == other.market
            && other.trigger_id == self.sibling_trigger_id
            && other.sibling_trigger_id == self.trigger_id
    }

    /// Wave 27a — check the slippage cap against the current oracle.
    /// Returns `true` if the cap is breached (caller should cancel
    /// instead of fire). `acceptable_price_ticks == 0` means "no cap"
    /// → returns `false`. Pure function for unit-testability.
    pub fn slippage_cap_breached(acceptable_price_ticks: u64, side: u8, oracle: u64) -> bool {
        if acceptable_price_ticks == 0 {
            return false;
        }
        match side {
            0 => oracle > acceptable_price_ticks, // long buying: cap = max admissible
            1 => oracle < acceptable_price_ticks, // short selling: cap = min admissible
            _ => false,
        }
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
    /// Phase 2f — same semantics as TriggerOrderAccountV3.sub_index.
    /// Every child slice the TWAP emits carries this sub_index in its
    /// RestingOrderV2.
    pub sub_index: u8,
    /// Wave 27b — same shape as `TriggerOrderAccountV3.acceptable_price_ticks`.
    /// Each TWAP slice is checked against this cap before injection.
    /// A slice that would fire while oracle is beyond the cap is
    /// **skipped** (not the TWAP deactivated) — the TWAP itself stays
    /// active so subsequent slices can fire if price returns within
    /// bounds. `0 = no cap` (legacy behavior).
    pub acceptable_price_ticks: u64,
    /// Reserved. Pre-Wave-27b TWAPs read this as zero from the
    /// trailing space().
    pub _reserved: [u8; 7],
}
impl TwapOrderAccountV3 {
    pub const SEED: &'static [u8] = b"twap_v3";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        // H-8 (audit 2026-06): body = 32+32 + 4×u8 + 8×u64 + 1 sub + 8 acceptable
        // + 7 reserved = 64+4+64+1+8+7 = 148 (the old comment miscounted as 144,
        // returning 152 < the 156 a full account needs → AccountDidNotSerialize on
        // a populated V3 TWAP). Correct size is 8 + 148.
        8 + 148
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
    /// Phase 2f — sub_index repurposes the first byte of the prior
    /// `_pad0: [u8; 4]`. Pre-Phase-2f accounts have this byte as 0
    /// (main TraderState) by virtue of the zero-initialised allocation,
    /// so the change is layout-compatible.
    pub sub_index: u8,
    pub _pad0: [u8; 3],
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
    /// Phase 2f — maker's sub-account index. Repurposed from the first
    /// byte of the prior `_pad0: [u8; 2]`. When the JIT offer fires
    /// against an underwater position, the synthetic close order picks
    /// up this sub_index so the maker rebate / position update lands
    /// on the right maker TraderState.
    pub maker_sub_index: u8,
    pub _pad0: [u8; 1],
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
    pub _pad0: [u8; 2],
    /// Lazer feed id, bound at config init when `source == SOURCE_LAZER`.
    /// AUDIT CR-2 fix: carved from the former `_pad0: [u8; 6]` (now `[u8; 2]`).
    /// Borsh field order + total size are preserved, so pre-existing configs
    /// (which had 6 zero pad bytes here) deserialize this as 0.
    pub lazer_feed_id: u32,
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
    /// AUDIT (2026-07): Lazer replay-nonce. The strictly-increasing
    /// publisher-attested microsecond timestamp of the last accepted Lazer
    /// price. `update_oracle_from_lazer` rejects any payload whose `timestamp_us`
    /// is not greater, so a public signed payload cannot be re-posted (or an
    /// older one applied) within the staleness window. APPENDED after `_pad1` —
    /// the account was always allocated 128 B (space) while the struct serialized
    /// to 96 B, so pre-existing configs deserialize this trailing field as 0
    /// (first Lazer update then seeds it). Borsh field order + offsets of every
    /// prior field are unchanged (same technique as the CR-2 `lazer_feed_id` carve).
    pub last_lazer_timestamp_us: u64,
}
impl MarketOracleConfigAccount {
    pub const SEED: &'static [u8] = b"oracle_config";
    pub const SOURCE_TRUSTED: u8 = 0;
    pub const SOURCE_PYTH: u8 = 1;
    pub const SOURCE_LAZER: u8 = 2;
    pub fn space() -> usize {
        // 8 disc
        //   + 1 bump + 1 source + 2 pad + 4 lazer_feed_id
        //   + 32 market + 32 feed_id
        //   + 4 + 4 max_staleness/conf
        //   + 1 tick_decimals + 7 pad
        //   + 8 last_lazer_timestamp_us
        // = 8 + 96 = 104. Allocation stays 128 (unchanged), so existing 128-byte
        // configs remain valid and the appended field reads 0.
        8 + 120
    }
}

#[cfg(test)]
mod oracle_config_layout_tests {
    use super::*;
    use anchor_lang::{AnchorDeserialize, AnchorSerialize};

    // AUDIT CR-2 regression: `lazer_feed_id: u32` was carved out of the former
    // `_pad0: [u8; 6]` (now `[u8; 2]`). Prove that an account serialized in the
    // OLD layout (6 zero pad bytes after `source`) still deserializes correctly
    // under the NEW struct, reading `lazer_feed_id == 0` and leaving every other
    // field byte-identical. This is what guarantees existing on-chain configs
    // are not corrupted by the field addition.
    #[test]
    fn old_layout_deserializes_with_zero_lazer_feed_id() {
        let market = Pubkey::new_unique();
        let feed = [7u8; 32];

        // OLD field order: bump,source,_pad0[6],market,pyth_feed,stale,conf,tick,_pad1[7]
        let mut old = Vec::new();
        old.push(9u8); // bump
        old.push(MarketOracleConfigAccount::SOURCE_PYTH); // source
        old.extend_from_slice(&[0u8; 6]); // _pad0 (old) — real accounts have zeros here
        old.extend_from_slice(market.as_ref());
        old.extend_from_slice(&feed);
        old.extend_from_slice(&123u32.to_le_bytes()); // max_staleness_seconds
        old.extend_from_slice(&45u32.to_le_bytes()); // max_confidence_bps
        old.push(3i8 as u8); // tick_decimals
        old.extend_from_slice(&[0u8; 7]); // _pad1
        // AUDIT (2026-07): the appended `last_lazer_timestamp_us: u64` reads from
        // the account's trailing allocation. A pre-existing on-chain config is 128
        // bytes (space) while the struct serialized to 88, so those 8 bytes are
        // zeros — simulate that slack here so the NEW struct has enough to decode.
        old.extend_from_slice(&[0u8; 8]); // last_lazer_timestamp_us slot (pre-existing = 0)

        let cfg = MarketOracleConfigAccount::try_from_slice(&old).unwrap();
        assert_eq!(cfg.bump, 9);
        assert_eq!(cfg.source, MarketOracleConfigAccount::SOURCE_PYTH);
        assert_eq!(cfg.lazer_feed_id, 0, "old configs must read lazer_feed_id as 0");
        assert_eq!(cfg.market, market);
        assert_eq!(cfg.pyth_price_feed_id, feed);
        assert_eq!(cfg.max_staleness_seconds, 123);
        assert_eq!(cfg.max_confidence_bps, 45);
        assert_eq!(cfg.tick_decimals, 3);
        assert_eq!(cfg.last_lazer_timestamp_us, 0, "old configs must read the nonce as 0");
        // Serialized struct is now 96 B (88 + 8 nonce); still well under the
        // unchanged 128-B `space()` allocation, so existing accounts stay valid.
        assert_eq!(old.len(), 96);
    }

    // A LAZER-bound config round-trips and exposes the new field.
    #[test]
    fn lazer_feed_id_round_trips() {
        let cfg = MarketOracleConfigAccount {
            bump: 1,
            source: MarketOracleConfigAccount::SOURCE_LAZER,
            _pad0: [0; 2],
            lazer_feed_id: 0xDEAD_BEEF,
            market: Pubkey::new_unique(),
            pyth_price_feed_id: [0; 32],
            max_staleness_seconds: 10,
            max_confidence_bps: 50,
            tick_decimals: -8,
            _pad1: [0; 7],
            last_lazer_timestamp_us: 1_700_000_000_000_000,
        };
        let mut bytes = Vec::new();
        AnchorSerialize::serialize(&cfg, &mut bytes).unwrap();
        let back = MarketOracleConfigAccount::try_from_slice(&bytes).unwrap();
        assert_eq!(back.lazer_feed_id, 0xDEAD_BEEF);
        assert_eq!(back.source, MarketOracleConfigAccount::SOURCE_LAZER);
        assert_eq!(back.last_lazer_timestamp_us, 1_700_000_000_000_000);
        assert_eq!(bytes.len(), 96, "serialized size is 96 (88 + 8 nonce); fits 128 space()");
    }
}

// ─── M-14 decentralization: sequencer committee (Phase 1) ───────────
//
/// The M-of-N validator committee that will authorize settlement once the
/// decentralized-sequencer endgame lands (see `docs/DECENTRALIZED_SEQUENCER.md`).
/// PHASE 1 is scaffolding: this account + its governance ix
/// (`set_sequencer_committee`) + the Kani-proven quorum logic
/// (`matcher::committee`) exist and are validated, but settlement authorization
/// is NOT yet generalized to it — so landing this changes no runtime behavior.
/// Seeds: `[b"seq_committee", market]`.
#[account]
#[derive(Debug)]
pub struct SequencerCommittee {
    pub market: Pubkey,
    pub bump: u8,
    /// N — number of active validators (`validators[..validator_count]`).
    pub validator_count: u8,
    /// M — signatures required. BFT-valid iff `3·threshold > 2·N` (quorum
    /// intersection); `N=1, threshold=1` is the backward-compatible single case.
    pub threshold: u8,
    pub _pad0: [u8; 5],
    /// Rotation epoch — bumped on every `set_sequencer_committee`.
    pub epoch: u64,
    /// The validator set. Only the first `validator_count` entries are live; the
    /// rest are `Pubkey::default()`.
    pub validators: [Pubkey; crate::constants::MAX_COMMITTEE_VALIDATORS],
    /// PHASE 2.6 — equivocation jail bitmask: bit `i` set ⇒ `validators[i]` was
    /// PROVABLY caught double-signing conflicting batches (`slash_equivocation`)
    /// and no longer counts toward a quorum. Carved from the former `_reserved`
    /// (backward-compatible: pre-existing committees read it as 0 = none jailed).
    pub jailed_mask: u64,
    pub _reserved: [u8; 56],
}
impl SequencerCommittee {
    pub const SEED: &'static [u8] = b"seq_committee";
    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 1 count + 1 threshold + 5 pad + 8 epoch
        //   + 32*MAX validators + 64 reserved
        8 + 32 + 1 + 1 + 1 + 5 + 8 + 32 * crate::constants::MAX_COMMITTEE_VALIDATORS + 64
    }
}

/// PHASE 2: the last committee-attested state transition per market. `commit_batch`
/// advances it when ≥`threshold` distinct validators sign a batch that chains onto
/// `last_state_root` with a strictly-greater `last_batch_seq`.
///
/// NOTE — this is NOT a batch auction (no FBA). Matching stays a CONTINUOUS
/// price-time CLOB; a "batch" here is only the unit of committed fills the
/// validator set threshold-signs (the consensus attestation over what the
/// continuous book already produced). Seeds: `[b"batch_attest", market]`.
#[account]
#[derive(Debug)]
pub struct BatchAttestation {
    pub market: Pubkey,
    pub bump: u8,
    pub _pad0: [u8; 7],
    /// Strictly-increasing committed batch sequence (replay/reorder guard — the
    /// batch analog of the per-fill `advance_settlement_seq`).
    pub last_batch_seq: u64,
    /// Committee epoch of the last accepted batch.
    pub epoch: u64,
    /// Chaining anchor: a batch must carry `prev_state_root == last_state_root`.
    pub last_state_root: [u8; 32],
    pub total_batches: u64,
    pub _reserved: [u8; 32],
}
impl BatchAttestation {
    pub const SEED: &'static [u8] = b"batch_attest";
    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 7 pad + 8 seq + 8 epoch + 32 root
        //   + 8 total + 32 reserved
        8 + 32 + 1 + 7 + 8 + 8 + 32 + 8 + 32
    }
}

// ─── Wave 24: H-haircut state ───────────────────────────────────────
//
// Sibling PDAs to the existing `MarketAccount` and `PositionAccount`.
// Additive — no legacy layout migration. See `docs/HAIRCUT_MATH.md`
// for the math and `matcher/haircut.rs` for the pure-function core.

/// Per-market haircut state. Tracks the global ratio inputs:
///   h = min(Residual, MaturedPosTotal) / MaturedPosTotal
///
/// Residual is **delta-tracked**: every money-moving ix (deposit, withdraw,
/// fee, liquidation, mature, convert) adjusts it incrementally rather
/// than scanning all accounts.
///
/// Seeds: `[b"haircut", market]`.
#[account]
#[derive(Debug)]
pub struct MarketHaircutStateAccount {
    pub market: Pubkey,
    pub bump: u8,
    pub _pad0: [u8; 7],
    /// V − C_tot − I from the spec, in quote lots. The excess of total
    /// protocol assets over committed trader collateral + insurance fund.
    /// This is what can back released positive PnL.
    pub residual_quote_lots: u128,
    /// Cumulative matured positive PnL awaiting conversion. The
    /// denominator for h. Decreases on convert.
    pub matured_pos_total_quote_lots: u128,
    /// Cumulative realized losses (informational; used for fee
    /// distribution to FLP in Wave 28).
    pub realized_loss_total_quote_lots: u128,
    /// Floor-rounding dust accrued from convert ops. Drained to
    /// insurance fund on `flush_haircut_dust`.
    pub dust_accrued_quote_lots: u128,
    /// Warmup window start, slots. Default 10.
    pub h_min_slots: u64,
    /// Warmup window end, slots. Default 200.
    pub h_max_slots: u64,
    /// Last slot at which `compute_h` was cached. The cached value is
    /// served by `view_haircut_ratio` to off-chain readers without
    /// requiring them to re-derive on every poll.
    pub h_scaled_cached: u64,
    pub h_cached_at_slot: u64,
    /// Reserved for future use (e.g. per-side residual decomposition).
    pub _reserved: [u8; 64],
}
impl MarketHaircutStateAccount {
    pub const SEED: &'static [u8] = b"haircut";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 7 pad
        //   + 16 + 16 + 16 + 16 + 8 + 8 + 8 + 8 + 64 reserved
        // = 8 + 200 = 208. Round to 240 for headroom.
        8 + 240
    }
}

/// Per-position haircut state. Sibling to `PositionAccount`. Lazy-init
/// on first realized positive PnL — flat-from-birth positions never
/// allocate one.
///
/// Seeds: `[b"position_haircut", market, position]`.
#[account]
#[derive(Debug)]
pub struct PositionHaircutStateAccount {
    pub market: Pubkey,
    pub position: Pubkey,
    pub bump: u8,
    pub _pad0: [u8; 7],
    /// Positive realized PnL waiting to mature. Adds on every gain;
    /// drains into `matured_pos_quote_lots` linearly over
    /// `[h_min, h_max]`.
    pub released_reserve_quote_lots: u64,
    /// Slot at which the *earliest* still-un-matured reserve dollar
    /// was added. Reset to 0 when the reserve fully drains.
    pub released_attached_at_slot: u64,
    /// Matured PnL awaiting conversion. Counts toward h's denominator.
    pub matured_pos_quote_lots: u64,
    /// Total reserve at warmup start. Required so `apply_mature` can
    /// compute matured-target-cumulative and stay idempotent at the
    /// same slot. Cleared to 0 when reserve fully drains.
    pub original_reserve_at_attach: u64,
    /// Reserved for future (e.g. per-position h_min override for
    /// premium accounts).
    pub _reserved: [u8; 24],
}
impl PositionHaircutStateAccount {
    pub const SEED: &'static [u8] = b"position_haircut";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 7 pad + 8 + 8 + 8 + 8 + 24 reserved = 136.
        // Round to 144.
        8 + 144
    }
}

// ─── Wave 25a: A/K/F/B per-side accrual state ───────────────────────
//
// Sibling PDA to MarketAccount. Holds the two-sided (long, short)
// A/K/F/B index quartet that drives O(1) per-position settlement.
// See `matcher::side_accrual` for the math and Percolator `spec.md`
// v12.20.6 §3 (Invariant 2) for the formal reference.
//
// Wave 25a (this commit) ships the account + init ix. Wave 25b rewires
// `settle_funding` / `auto_deleverage` to operate on it. Wave 25c adds
// Position snapshots `(a_snap, k_snap, f_snap, b_snap)` and rewires
// `apply_fill_to_position` to capture them on attach.
//
// Seeds: `[b"side_accrual", market]`.

/// Per-market side accrual state. Holds A/K/F/B indices for long and
/// short sides plus the per-side state machine (Normal / DrainOnly /
/// ResetPending).
///
/// Storage layout is dictated by `matcher::side_accrual::SideAccrual`
/// being u128 / i128 fields → 16 bytes each. Two sides × 4 indices ×
/// 16 = 128 bytes for indices alone. Plus mode (1), epoch (4),
/// slot_last (8), price_last (8) per side. Total body ~250 bytes.
#[account]
#[derive(Debug)]
pub struct MarketSideAccrualAccount {
    pub market: Pubkey,
    pub bump: u8,
    pub _pad0: [u8; 7],

    // ─── Long side ──
    pub long_a: u128,
    pub long_k: i128,
    pub long_f: i128,
    pub long_b: i128,
    pub long_mode: u8, // 0=Normal, 1=DrainOnly, 2=ResetPending
    pub long_epoch: u32,
    pub _long_pad: [u8; 3],
    pub long_slot_last: u64,
    pub long_price_last: u64,

    // ─── Short side ──
    pub short_a: u128,
    pub short_k: i128,
    pub short_f: i128,
    pub short_b: i128,
    pub short_mode: u8,
    pub short_epoch: u32,
    pub _short_pad: [u8; 3],
    pub short_slot_last: u64,
    pub short_price_last: u64,

    /// Reserved for future expansion (e.g. per-side bankruptcy chunk
    /// counters, ADL ranking tiebreakers).
    pub _reserved: [u8; 64],
}

impl MarketSideAccrualAccount {
    pub const SEED: &'static [u8] = b"side_accrual";
    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 7 pad
        //   + per side: 16*4 (A/K/F/B) + 1 mode + 4 epoch + 3 pad + 8 slot + 8 price = 96
        //   × 2 sides = 192
        //   + 64 reserved
        // = 8 + 296 = 304. Round up to 320 for headroom.
        8 + 320
    }

    /// Hydrate the `matcher::side_accrual::SideAccrual` struct for one
    /// side. Used by Wave 25b wire-in points that operate on the pure
    /// `SideAccrual` type without binding to Anchor accounts.
    pub fn long_side(&self) -> crate::matcher::side_accrual::SideAccrual {
        use crate::matcher::side_accrual::*;
        SideAccrual {
            a: self.long_a,
            k: self.long_k,
            f: self.long_f,
            b: self.long_b,
            mode: match self.long_mode {
                0 => SideMode::Normal,
                1 => SideMode::DrainOnly,
                2 => SideMode::ResetPending,
                _ => SideMode::Normal,
            },
            epoch: self.long_epoch,
            slot_last: self.long_slot_last,
            price_last: self.long_price_last,
        }
    }

    pub fn short_side(&self) -> crate::matcher::side_accrual::SideAccrual {
        use crate::matcher::side_accrual::*;
        SideAccrual {
            a: self.short_a,
            k: self.short_k,
            f: self.short_f,
            b: self.short_b,
            mode: match self.short_mode {
                0 => SideMode::Normal,
                1 => SideMode::DrainOnly,
                2 => SideMode::ResetPending,
                _ => SideMode::Normal,
            },
            epoch: self.short_epoch,
            slot_last: self.short_slot_last,
            price_last: self.short_price_last,
        }
    }

    /// Write back a side's state. Counterpart to `long_side()` /
    /// `short_side()` — round-tripping `read → mutate → write` is the
    /// idiomatic wire-in pattern for Wave 25b.
    pub fn write_long_side(&mut self, s: &crate::matcher::side_accrual::SideAccrual) {
        self.long_a = s.a;
        self.long_k = s.k;
        self.long_f = s.f;
        self.long_b = s.b;
        self.long_mode = s.mode as u8;
        self.long_epoch = s.epoch;
        self.long_slot_last = s.slot_last;
        self.long_price_last = s.price_last;
    }

    pub fn write_short_side(&mut self, s: &crate::matcher::side_accrual::SideAccrual) {
        self.short_a = s.a;
        self.short_k = s.k;
        self.short_f = s.f;
        self.short_b = s.b;
        self.short_mode = s.mode as u8;
        self.short_epoch = s.epoch;
        self.short_slot_last = s.slot_last;
        self.short_price_last = s.price_last;
    }
}

// ─── Wave 26a: Per-market envelope config ───────────────────────────
//
// Stores the per-slot price/funding envelope parameters proved at init
// via `matcher::envelope::prove_envelope`. Once written, the engine
// can call `gate_price_move` against this account on every mark
// advance to enforce the per-slot solvency bound.
//
// Sibling to MarketAccount; additive — no layout migration. Seeds:
// `[b"envelope", market]`.
//
// Wave 26a (this commit) lands the storage + set/verify ix. Wave 26b
// hooks the runtime gate into apply_fill's mark-EMA advance alongside
// Wave 25b's settle_funding rewrite.

#[account]
#[derive(Debug)]
pub struct MarketEnvelopeConfigAccount {
    pub market: Pubkey,
    pub bump: u8,
    pub _pad0: [u8; 7],

    /// Max per-slot oracle price move (bps of previous price).
    pub max_price_move_bps_per_slot: u32,
    /// Max accrual window a single call can advance K/F over (slots).
    pub max_accrual_dt_slots: u64,
    /// Max absolute funding rate per slot (scaled by 10^9).
    pub max_abs_funding_e9_per_slot: i64,
    /// Maintenance margin requirement (bps of notional).
    pub maintenance_bps: u32,
    /// Liquidation fee (bps of liquidation notional).
    pub liquidation_fee_bps: u32,
    /// Absolute floor on liquidation fee (quote lots).
    pub min_liquidation_abs_lots: u64,
    /// Absolute floor on maintenance margin requirement (quote lots).
    pub min_nonzero_mm_req_lots: u64,

    /// Slot at which params were last set / proven. Off-chain checkers
    /// can detect param updates by watching this value bump.
    pub last_proven_at_slot: u64,
    /// Monotonically increasing version counter, bumped on every
    /// successful `set_envelope_config` call.
    pub version: u32,
    pub _pad1: [u8; 4],

    /// Wave 26b — runtime gate state. Tracks the (slot, price) at which
    /// the engine last observed an oracle update on this market. Used
    /// to compute `dt_slots` and `|Δp|` for `gate_price_move`. Updated
    /// after every successful oracle update on opted-in markets.
    /// `last_observed_slot = 0` means "no prior observation"; the
    /// first oracle update on a freshly-init'd envelope skips the
    /// gate (matches `gate_price_move`'s `p_last == 0` semantics).
    pub last_observed_slot: u64,
    pub last_observed_price_ticks: u64,
    /// Counters for observability. `gate_passes` increments on every
    /// successful gate; `gate_rejects` on every rejection (the ix
    /// reverts on reject, so this counter only bumps when the
    /// transaction succeeded *despite* the gate — i.e., never with
    /// the current strict-reject design. Reserved for future
    /// soft-gate modes.).
    pub gate_passes: u64,
    pub gate_rejects: u64,

    /// GOVERNANCE Phase-3 (2026-07): one-way oracle-source lock. `1` = the direct
    /// authority `update_oracle` / `update_oracle_quorum` paths are DISABLED for this
    /// market (only the Pyth / Lazer paths are accepted) — removing the compromised-
    /// authority "walk the mark within the H-6 per-slot cap" vector entirely. Carved
    /// from `_reserved` so the account size is unchanged and pre-existing envelopes
    /// read it back as `0` (unlocked). Set (never cleared — one-way) by
    /// `lock_oracle_source`; enforced in the two direct-write handlers, which already
    /// REQUIRE this account (H-6), so the lock cannot be bypassed by omitting it.
    pub source_locked: u8,
    pub _reserved: [u8; 31],
}

impl MarketEnvelopeConfigAccount {
    pub const SEED: &'static [u8] = b"envelope";

    pub fn space() -> usize {
        // 8 disc + 32 market + 1 bump + 7 pad
        //   + 4 + 8 + 8 + 4 + 4 + 8 + 8 = 44 (params)
        //   + 8 + 4 + 4 pad = 16 (version block)
        //   + 8 + 8 + 8 + 8 = 32 (Wave 26b gate state)
        //   + 32 reserved
        // = 8 + 32 + 1 + 7 + 44 + 16 + 32 + 32 = 172. Round to 192.
        8 + 192
    }

    /// Hydrate the pure `matcher::envelope::EnvelopeParams` struct for
    /// `prove_envelope` / `gate_price_move` use. Mirror of
    /// `MarketSideAccrualAccount::long_side()` pattern.
    pub fn params(&self) -> crate::matcher::envelope::EnvelopeParams {
        crate::matcher::envelope::EnvelopeParams {
            max_price_move_bps_per_slot: self.max_price_move_bps_per_slot,
            max_accrual_dt_slots: self.max_accrual_dt_slots,
            max_abs_funding_e9_per_slot: self.max_abs_funding_e9_per_slot,
            maintenance_bps: self.maintenance_bps,
            liquidation_fee_bps: self.liquidation_fee_bps,
            min_liquidation_abs_lots: self.min_liquidation_abs_lots,
            min_nonzero_mm_req_lots: self.min_nonzero_mm_req_lots,
        }
    }

    /// Write-back counterpart. Caller is responsible for `prove_envelope`
    /// validation BEFORE calling this (the on-chain ix enforces it).
    pub fn write_params(&mut self, p: &crate::matcher::envelope::EnvelopeParams) {
        self.max_price_move_bps_per_slot = p.max_price_move_bps_per_slot;
        self.max_accrual_dt_slots = p.max_accrual_dt_slots;
        self.max_abs_funding_e9_per_slot = p.max_abs_funding_e9_per_slot;
        self.maintenance_bps = p.maintenance_bps;
        self.liquidation_fee_bps = p.liquidation_fee_bps;
        self.min_liquidation_abs_lots = p.min_liquidation_abs_lots;
        self.min_nonzero_mm_req_lots = p.min_nonzero_mm_req_lots;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AUDIT HIGH-7 (2026-07): host coverage for the OCO sibling validation —
    // the security-critical check that stops a malicious executor from passing
    // an unrelated trigger just to deactivate it.
    fn mk_trig(trader: Pubkey, market: Pubkey, id: u8, sibling: u8) -> TriggerOrderAccountV3 {
        TriggerOrderAccountV3 {
            trader,
            market,
            bump: 0,
            trigger_id: id,
            side: 0,
            kind: 0,
            flags: 0,
            size_lots: 0,
            trigger_price_ticks: 0,
            limit_price_ticks: 0,
            created_at_slot: 0,
            expires_at_slot: 0,
            sub_index: 0,
            acceptable_price_ticks: 0,
            sibling_trigger_id: sibling,
            _reserved: [0; 7],
        }
    }

    #[test]
    fn oco_accepts_genuine_mutual_pair() {
        let t = Pubkey::new_from_array([1u8; 32]);
        let m = Pubkey::new_from_array([2u8; 32]);
        let tp = mk_trig(t, m, 1, 2); // TP: id 1, points at 2
        let sl = mk_trig(t, m, 2, 1); // SL: id 2, points at 1
        assert!(tp.is_oco_sibling(&sl));
        assert!(sl.is_oco_sibling(&tp));
    }

    #[test]
    fn oco_rejects_non_mutual_backlink() {
        let t = Pubkey::new_from_array([1u8; 32]);
        let m = Pubkey::new_from_array([2u8; 32]);
        let tp = mk_trig(t, m, 1, 2);
        // Attacker-owned trigger with the right id (2) but NOT pointing back at
        // TP (its sibling is 9) — must be rejected.
        let fake = mk_trig(t, m, 2, 9);
        assert!(!tp.is_oco_sibling(&fake));
    }

    #[test]
    fn oco_rejects_wrong_trader_market_or_id() {
        let t = Pubkey::new_from_array([1u8; 32]);
        let m = Pubkey::new_from_array([2u8; 32]);
        let tp = mk_trig(t, m, 1, 2);
        assert!(!tp.is_oco_sibling(&mk_trig(Pubkey::new_from_array([9u8; 32]), m, 2, 1)));
        assert!(!tp.is_oco_sibling(&mk_trig(t, Pubkey::new_from_array([9u8; 32]), 2, 1)));
        assert!(!tp.is_oco_sibling(&mk_trig(t, m, 3, 1))); // id 3 != expected sibling 2
    }

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
        assert_ne!(jit, MarketHaircutStateAccount::SEED);
        assert_ne!(jit, PositionHaircutStateAccount::SEED);
    }

    /// H-8 (audit 2026-06) REGRESSION: `space()` must cover the FULL Borsh
    /// serialization of a populated account, not an undercounted body. The bug
    /// returned 8+144 while the real body is 148 bytes → AccountDidNotSerialize
    /// on a fully-populated V3 TWAP (e.g. one carrying `acceptable_price_ticks`).
    /// Unlike the sibling `>= 8 + body` checks, this pins the EXACT serialized
    /// length so any future field addition that desyncs `space()` fails loudly.
    #[test]
    fn twap_v3_space_matches_borsh_serialized_len() {
        use anchor_lang::AnchorSerialize;
        let acc = TwapOrderAccountV3 {
            trader: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            bump: 255,
            twap_id: 7,
            side: 1,
            flags: TwapOrderAccountV3::FLAG_ACTIVE,
            slice_size_lots: u64::MAX,
            total_size_lots: u64::MAX,
            size_executed_lots: 123,
            limit_price_ticks: 456,
            start_slot: 1,
            slot_interval: 2,
            end_slot: 3,
            last_slice_at_slot: 4,
            sub_index: 9,
            acceptable_price_ticks: u64::MAX,
            _reserved: [0xAB; 7],
        };
        // borsh 1.x (anchor 1.x) removed the inherent `try_to_vec`; serialize via
        // AnchorSerialize into a buffer instead.
        let mut body = Vec::new();
        acc.serialize(&mut body).expect("borsh serialize");
        assert_eq!(body.len(), 148, "serialized TWAP body must be 148 bytes");
        assert_eq!(
            TwapOrderAccountV3::space(),
            8 + body.len(),
            "space() must equal 8-byte disc + exact serialized body"
        );
    }

    #[test]
    fn haircut_seeds_are_stable_and_distinct() {
        assert_eq!(MarketHaircutStateAccount::SEED, b"haircut");
        assert_eq!(PositionHaircutStateAccount::SEED, b"position_haircut");
        assert_ne!(
            MarketHaircutStateAccount::SEED,
            PositionHaircutStateAccount::SEED
        );
    }

    #[test]
    fn haircut_space_fits_layout() {
        // Body (excl. 8 disc):
        //   market(32) + bump(1) + pad(7) + 4×u128(64) + 4×u64(32) + reserved(64) = 200
        assert!(MarketHaircutStateAccount::space() >= 8 + 200);
        // Body:
        //   market(32) + position(32) + bump(1) + pad(7) + 3×u64(24) + reserved(32) = 128
        assert!(PositionHaircutStateAccount::space() >= 8 + 128);
    }

    #[test]
    fn side_accrual_seed_is_stable() {
        assert_eq!(MarketSideAccrualAccount::SEED, b"side_accrual");
    }

    #[test]
    fn side_accrual_seed_distinct() {
        assert_ne!(
            MarketSideAccrualAccount::SEED,
            MarketHaircutStateAccount::SEED
        );
        assert_ne!(
            MarketSideAccrualAccount::SEED,
            PositionHaircutStateAccount::SEED
        );
    }

    #[test]
    fn side_accrual_space_fits_layout() {
        // Body (excl. 8 disc): see space() comment. 296 minimum.
        assert!(MarketSideAccrualAccount::space() >= 8 + 296);
    }

    #[test]
    fn envelope_config_seed_distinct() {
        assert_eq!(MarketEnvelopeConfigAccount::SEED, b"envelope");
        assert_ne!(
            MarketEnvelopeConfigAccount::SEED,
            MarketSideAccrualAccount::SEED
        );
        assert_ne!(
            MarketEnvelopeConfigAccount::SEED,
            MarketHaircutStateAccount::SEED
        );
    }

    #[test]
    fn envelope_config_space_fits_layout() {
        // Body (excl. 8 disc):
        //   market(32) + bump(1) + pad(7) + scalars(44) + version_block(16)
        //   + gate_state(32) + reserved(32) = 164
        assert!(MarketEnvelopeConfigAccount::space() >= 8 + 164);
    }

    #[test]
    fn envelope_config_round_trips() {
        use crate::matcher::envelope::EnvelopeParams;
        let mut acc = MarketEnvelopeConfigAccount {
            market: Pubkey::default(),
            bump: 0,
            _pad0: [0; 7],
            max_price_move_bps_per_slot: 0,
            max_accrual_dt_slots: 0,
            max_abs_funding_e9_per_slot: 0,
            maintenance_bps: 0,
            liquidation_fee_bps: 0,
            min_liquidation_abs_lots: 0,
            min_nonzero_mm_req_lots: 0,
            last_proven_at_slot: 0,
            version: 0,
            _pad1: [0; 4],
            last_observed_slot: 0,
            last_observed_price_ticks: 0,
            gate_passes: 0,
            gate_rejects: 0,
            source_locked: 0,
            _reserved: [0; 31],
        };
        let p = EnvelopeParams::default();
        acc.write_params(&p);
        let q = acc.params();
        assert_eq!(p, q, "round-trip preserves all fields");
        // Wave 26b gate state defaults to zero (no prior observation).
        assert_eq!(acc.last_observed_slot, 0);
        assert_eq!(acc.last_observed_price_ticks, 0);
        assert_eq!(acc.gate_passes, 0);
        assert_eq!(acc.gate_rejects, 0);
    }

    #[test]
    fn side_accrual_round_trips() {
        use crate::matcher::side_accrual::*;
        let mut acc = MarketSideAccrualAccount {
            market: Pubkey::default(),
            bump: 0,
            _pad0: [0; 7],
            long_a: ADL_ONE,
            long_k: 0,
            long_f: 0,
            long_b: 0,
            long_mode: 0,
            long_epoch: 0,
            _long_pad: [0; 3],
            long_slot_last: 0,
            long_price_last: 0,
            short_a: ADL_ONE,
            short_k: 0,
            short_f: 0,
            short_b: 0,
            short_mode: 0,
            short_epoch: 0,
            _short_pad: [0; 3],
            short_slot_last: 0,
            short_price_last: 0,
            _reserved: [0; 64],
        };
        // Round trip on the long side.
        let mut s = acc.long_side();
        assert_eq!(s.a, ADL_ONE);
        s.a = MIN_A_SIDE / 2;
        s.k = 42;
        s.mode = SideMode::DrainOnly;
        s.epoch = 7;
        acc.write_long_side(&s);
        assert_eq!(acc.long_a, MIN_A_SIDE / 2);
        assert_eq!(acc.long_k, 42);
        assert_eq!(acc.long_mode, 1);
        assert_eq!(acc.long_epoch, 7);
        // Short side untouched.
        assert_eq!(acc.short_a, ADL_ONE);
        assert_eq!(acc.short_mode, 0);
    }
}
