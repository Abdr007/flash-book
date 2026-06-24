//! Stop-limit composite order (Wave 55).
//!
//! Combines trigger semantics with explicit limit + slippage:
//! - `trigger_price` — fires when oracle crosses this in the
//!   configured direction.
//! - `limit_price` — the resulting order's price.
//! - `max_slippage_bps` — runtime gate: if oracle has moved past
//!   `oracle_at_fire × (1 ± slippage)` between fire decision and
//!   limit injection, abort. Defense against gappy fills.
//!
//! This is the most defensive trigger variant — gives the trader full
//! control over both the firing condition AND the realized fill price.

use crate::constants::BPS_DENOM;

/// Verify slippage hasn't been breached between fire time and exec
/// time. Returns `true` if the order should still inject; `false`
/// if it should cancel.
pub fn slippage_ok(
    oracle_at_fire_ticks: u64,
    oracle_now_ticks: u64,
    side: u8,
    max_slippage_bps: u32,
) -> bool {
    if max_slippage_bps == 0 {
        return true; // no cap
    }
    let abs_delta = if oracle_now_ticks >= oracle_at_fire_ticks {
        oracle_now_ticks - oracle_at_fire_ticks
    } else {
        oracle_at_fire_ticks - oracle_now_ticks
    };
    let max_delta = ((oracle_at_fire_ticks as u128)
        .saturating_mul(max_slippage_bps as u128)
        / (BPS_DENOM as u128))
        .min(u64::MAX as u128) as u64;
    // For BUY: only "up" moves cost the trader.
    // For SELL: only "down" moves cost the trader.
    match side {
        0 => {
            // Buy: rejected if oracle moved up by more than slip.
            if oracle_now_ticks <= oracle_at_fire_ticks {
                true
            } else {
                abs_delta <= max_delta
            }
        }
        1 => {
            // Sell: rejected if oracle moved down by more than slip.
            if oracle_now_ticks >= oracle_at_fire_ticks {
                true
            } else {
                abs_delta <= max_delta
            }
        }
        _ => false,
    }
}

/// Validate the user-supplied trigger + limit pair at place-time.
/// Rules:
/// - For a long entry (side=0, kind=1 take-profit equiv): limit ≥ trigger.
/// - For a short entry (side=1, kind=0 stop equiv): limit ≤ trigger.
/// (Side convention: 0=buy/long order, 1=sell/short order.)
///
/// `kind=0` triggers when oracle ≤ trigger_price (e.g. stop-loss for
/// long, breakout for short).
/// `kind=1` triggers when oracle ≥ trigger_price (e.g. take-profit
/// for long, stop-loss for short).
pub fn valid_place_params(
    side: u8,
    kind: u8,
    trigger_price_ticks: u64,
    limit_price_ticks: u64,
) -> bool {
    if trigger_price_ticks == 0 || limit_price_ticks == 0 {
        return false;
    }
    match (side, kind) {
        // Stop-loss for long (sell on down move): limit should be ≤ trigger.
        (1, 0) => limit_price_ticks <= trigger_price_ticks,
        // Take-profit for long (sell on up move): limit ≥ trigger.
        (1, 1) => limit_price_ticks >= trigger_price_ticks,
        // Stop-loss for short (buy on up move): limit ≥ trigger.
        (0, 1) => limit_price_ticks >= trigger_price_ticks,
        // Take-profit for short (buy on down move): limit ≤ trigger.
        (0, 0) => limit_price_ticks <= trigger_price_ticks,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippage_zero_admits_all() {
        assert!(slippage_ok(100, 200, 0, 0));
        assert!(slippage_ok(100, 50, 1, 0));
    }

    #[test]
    fn buy_admits_oracle_dropping() {
        // BUY: oracle drops between fire and exec → good for trader, admit.
        assert!(slippage_ok(100, 50, 0, 100));
    }

    #[test]
    fn buy_admits_small_oracle_up_move() {
        // BUY: 1% slippage allowed. Oracle 100 → 100.5 = 50 bps move. OK.
        assert!(slippage_ok(10_000, 10_050, 0, 100));
    }

    #[test]
    fn buy_rejects_oversized_oracle_up_move() {
        // BUY: 100 bps slippage. Oracle 10_000 → 10_200 = 200 bps move.
        assert!(!slippage_ok(10_000, 10_200, 0, 100));
    }

    #[test]
    fn sell_admits_oracle_rising() {
        // SELL: oracle rises → good for trader.
        assert!(slippage_ok(100, 200, 1, 100));
    }

    #[test]
    fn sell_rejects_oversized_oracle_down() {
        assert!(!slippage_ok(10_000, 9_800, 1, 100));
    }

    #[test]
    fn place_params_stop_loss_long_correct() {
        // Stop-loss long: sell when oracle drops; limit ≤ trigger.
        assert!(valid_place_params(1, 0, 100, 95));
        assert!(valid_place_params(1, 0, 100, 100));
        assert!(!valid_place_params(1, 0, 100, 105));
    }

    #[test]
    fn place_params_take_profit_long_correct() {
        // TP long: sell when oracle rises; limit ≥ trigger.
        assert!(valid_place_params(1, 1, 100, 105));
        assert!(!valid_place_params(1, 1, 100, 95));
    }

    #[test]
    fn place_params_stop_loss_short_correct() {
        // SL short: buy when oracle rises; limit ≥ trigger.
        assert!(valid_place_params(0, 1, 100, 105));
        assert!(!valid_place_params(0, 1, 100, 95));
    }

    #[test]
    fn place_params_rejects_zeros() {
        assert!(!valid_place_params(0, 0, 0, 100));
        assert!(!valid_place_params(0, 0, 100, 0));
    }
}
