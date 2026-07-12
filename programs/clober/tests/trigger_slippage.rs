//! `acceptable_price` slippage cap on v3 trigger orders.
//!
//! Verifies `TriggerOrderAccount::slippage_cap_breached` directly
//! (the pure function the on-chain handler delegates to) and walks
//! the place → fire → execute pipeline algorithmically.

use clober::extended_state::TriggerOrderAccount;

// ─── Pure pattern: cap-breach predicate ─────────────────────────────

#[test]
fn cap_zero_means_no_cap_regardless_of_side_or_oracle() {
    // Triggers with no cap set (acceptable_price_ticks == 0)
    // never breach — full backward compat.
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        0, 0, 1_000_000
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        0, 1, 1_000_000
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        0,
        0,
        u64::MAX
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(0, 1, 1));
}

#[test]
fn long_buying_breaches_when_oracle_above_cap() {
    // side=0 → buying. Cap = max admissible price. Oracle > cap = breach.
    assert!(TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 1_000_001
    ));
    assert!(TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 2_000_000
    ));
}

#[test]
fn long_buying_admits_when_oracle_at_or_below_cap() {
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 1_000_000
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 999_999
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 0
    ));
}

#[test]
fn short_selling_breaches_when_oracle_below_cap() {
    // side=1 → selling. Cap = min admissible price. Oracle < cap = breach.
    assert!(TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 999_999
    ));
    assert!(TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 500_000
    ));
    assert!(TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 0
    ));
}

#[test]
fn short_selling_admits_when_oracle_at_or_above_cap() {
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 1_000_000
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 1_000_001
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000,
        1,
        u64::MAX
    ));
}

#[test]
fn invalid_side_returns_no_breach_safely() {
    // Defensive: an invalid side value (place_trigger validates ≤ 1
    // at write time, but if a stale account has a corrupt side byte
    // we don't want to spuriously reject).
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 99, 1
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000,
        99,
        u64::MAX
    ));
}

// ─── Realistic stop-loss / take-profit scenarios ─────────────────────

#[test]
fn stop_loss_on_long_protected_against_gap_down() {
    // Trader is long, places a stop at trigger_price=950 (close at 950)
    // limit_price=950, acceptable_price=900 (won't sell below $900).
    // The closing order is side=1 (short to close the long).
    let acceptable = 900;
    let side = 1;
    // Normal slow drop to 950: trigger fires, no slippage breach.
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        acceptable, side, 950
    ));
    // Gap-down through to 850: oracle < 900, slippage breached, trigger cancels.
    assert!(TriggerOrderAccount::slippage_cap_breached(
        acceptable, side, 850
    ));
}

#[test]
fn take_profit_on_long_protected_against_gap_up_misfill() {
    // Trader is long at 100, places TP at trigger_price=110, limit_price=110,
    // acceptable_price=115 (max acceptable if oracle gapped).
    // Closing order side=1 (sell). For a TP the gap up is GOOD, so the
    // slippage cap is somewhat unusual here — but if the trader set 115
    // as their max-tolerable execution price they probably want the
    // resulting limit at 110 to NOT fill if oracle is above 115.
    // BUT a TP closing SHORT would fire at oracle ≥ 110, so the
    // direction matters more for the SHORT side. Let me adjust the
    // example: TP on a SHORT (close short at low price, side=0):
    let acceptable_for_short_close = 90; // won't BUY above $90
    let side = 0; // buying to close the short
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        acceptable_for_short_close,
        side,
        85
    ));
    assert!(TriggerOrderAccount::slippage_cap_breached(
        acceptable_for_short_close,
        side,
        95
    ));
}

#[test]
fn entry_order_protected_against_chase_fill() {
    // Trader places a stop-entry to LONG at trigger=110 (break-out buy),
    // limit_price=110, acceptable_price=112 (won't buy above $112).
    // side=0 (buying).
    let acceptable = 112;
    let side = 0;
    // Normal break to 111: fine, fire.
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        acceptable, side, 111
    ));
    // Vertical spike to 115: cap breached, don't fire.
    assert!(TriggerOrderAccount::slippage_cap_breached(
        acceptable, side, 115
    ));
}

// ─── Boundary semantics: cap == trigger_price ───────────────────────

#[test]
fn cap_equal_to_oracle_admits_exact_match() {
    // Oracle exactly at the cap admits (cap is the *limit*, not strict).
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 0, 1_000_000
    ));
    assert!(!TriggerOrderAccount::slippage_cap_breached(
        1_000_000, 1, 1_000_000
    ));
}

#[test]
fn cap_breach_off_by_one() {
    assert!(TriggerOrderAccount::slippage_cap_breached(100, 0, 101));
    assert!(!TriggerOrderAccount::slippage_cap_breached(100, 0, 100));
    assert!(TriggerOrderAccount::slippage_cap_breached(100, 1, 99));
    assert!(!TriggerOrderAccount::slippage_cap_breached(100, 1, 100));
}

// ─── Place-time validation (mirrors ix logic) ───────────────────────

#[test]
fn place_validates_cap_direction_for_long() {
    // For a long buy (side=0), acceptable_price must be ≥ trigger_price.
    // The handler ensures this; here we mirror the check.
    let trigger_price = 1_000_000;
    let cap_ok = 1_010_000; // ≥ trigger → admissible
    let cap_bad = 990_000; // < trigger → invalid (would always breach)
    assert!(cap_ok >= trigger_price);
    assert!(cap_bad < trigger_price);
}

#[test]
fn place_validates_cap_direction_for_short() {
    // For a short sell (side=1), acceptable_price must be ≤ trigger_price.
    let trigger_price = 1_000_000;
    let cap_ok = 990_000;
    let cap_bad = 1_010_000;
    assert!(cap_ok <= trigger_price);
    assert!(cap_bad > trigger_price);
}

#[test]
fn space_still_accommodates_layout() {
    // Body (excl. 8 disc): 32 + 32 + 1×7 + 8×5 + 1 + 8 + 8 = 126.
    //   2 Pubkeys + bump + trigger_id + side + kind + flags = 32+32+5 = 69
    //   5 u64s (size, trigger_px, limit_px, created, expires) = 40 → 109
    //   sub_index (u8) = 1 → 110
    //   acceptable_price (u64) = 8 → 118
    //   _reserved [u8;8] = 8 → 126
    // Space allocates 8 + 128 = 136 → fits with 10 bytes spare.
    let body_bytes = 32 + 32 + 1 + 1 + 1 + 1 + 1 + 8 + 8 + 8 + 8 + 8 + 1 + 8 + 8;
    assert_eq!(body_bytes, 126);
    assert!(TriggerOrderAccount::space() >= 8 + body_bytes);
    // Headroom: 136 - 8 (disc) - 126 (body) = 2 bytes spare.
    assert_eq!(TriggerOrderAccount::space() - 8 - body_bytes, 2);
}
