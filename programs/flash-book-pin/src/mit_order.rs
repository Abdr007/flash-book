//! Market-If-Touched (MIT) order pricing (Wave 51).
//!
//! Like a trigger order, but the resulting order is **market-style**:
//! when the oracle crosses the trigger, the order injects at the
//! best-touchable price with a slippage cap. The trader doesn't have
//! to pre-set a limit price they hope the market reaches.
//!
//! Better for retail UX than plain triggers: "buy 1 SOL when price
//! hits $150" is simpler to reason about than "buy 1 SOL @ $150.50
//! when price hits $150" (regular trigger).
//!
//! The trader still sets `max_slippage_bps` to bound risk. The MIT
//! resolves to a limit at `oracle × (1 ± slippage)` at fire time.

use crate::constants::BPS_DENOM;

/// Compute the resulting limit price for a MIT firing at the given
/// oracle, side, and slippage tolerance.
///
/// `side`: 0 = buy (long), 1 = sell (short).
/// `slippage_bps`: max tolerated slippage in bps.
///
/// For a BUY: limit = oracle × (1 + slippage_bps / 10_000).
/// For a SELL: limit = oracle × (1 - slippage_bps / 10_000).
///
/// Returns `None` on overflow or invalid side.
pub fn resolve_mit_limit_ticks(
    oracle_price_ticks: u64,
    side: u8,
    slippage_bps: u32,
) -> Option<u64> {
    if oracle_price_ticks == 0 || slippage_bps as u64 > BPS_DENOM as u64 {
        return None;
    }
    let scaled = (oracle_price_ticks as u128).checked_mul(slippage_bps as u128)?;
    let slip_amount = scaled / (BPS_DENOM as u128);
    match side {
        0 => oracle_price_ticks.checked_add(slip_amount.min(u64::MAX as u128) as u64),
        1 => oracle_price_ticks.checked_sub(slip_amount.min(u64::MAX as u128) as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buy_resolves_above_oracle() {
        // 100 bps = 1% slip → 1000_000 × 0.01 = 10_000 added.
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 0, 100), Some(1_010_000));
    }

    #[test]
    fn sell_resolves_below_oracle() {
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 1, 100), Some(990_000));
    }

    #[test]
    fn zero_slippage_returns_oracle() {
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 0, 0), Some(1_000_000));
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 1, 0), Some(1_000_000));
    }

    #[test]
    fn excessive_slippage_rejected() {
        // Slippage > 100% → reject.
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 0, 20_000), None);
    }

    #[test]
    fn zero_oracle_returns_none() {
        assert_eq!(resolve_mit_limit_ticks(0, 0, 100), None);
    }

    #[test]
    fn invalid_side_returns_none() {
        assert_eq!(resolve_mit_limit_ticks(1_000_000, 99, 100), None);
    }

    #[test]
    fn sell_underflow_handled() {
        // 100% slippage on a SELL would underflow; rejected via
        // slippage cap above. But within bounds: 1% slip on price 100
        // → limit = 99.
        assert_eq!(resolve_mit_limit_ticks(100, 1, 100), Some(99));
    }
}
