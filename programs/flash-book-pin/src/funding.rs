//! Funding math — faithful transcription of matcher::funding::funding_owed
//! (Q64.64 cumulative-index model). Pure, host-tested for equivalence.
const FRACTIONAL_BITS: u32 = 64;

/// owed = sign(long?+1:-1) * notional * (cum_now - cum_at_entry) >> 64
pub fn funding_owed(is_long: bool, notional_quote_lots: u64, cum_now: i128, cum_at_entry: i128) -> Option<i128> {
    let delta = cum_now.checked_sub(cum_at_entry)?;
    let sign: i128 = if is_long { 1 } else { -1 };
    let prod = (notional_quote_lots as i128).checked_mul(delta)?;
    let scaled = prod >> FRACTIONAL_BITS;
    Some(sign * scaled)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FundingSettleErr { Overflow, ResidualUnderflow }

/// Settle a position's accrued funding on its PRE-trade size against the current
/// cumulative index `cum_now`, fold it into the right collateral bucket (isolated
/// = the position's own collateral, cross = `*ts_collateral`), move the haircut
/// `*residual` so `Δcollateral == −Δresidual` (RISK-1), then RE-STAMP the entry
/// index. Idempotent: a position already at `cum_now` owes 0 and is unchanged.
/// Notional is MARK-priced (`mark_price_ticks × tick_size`), matching the risk
/// engine's funding term — NOT the fill price.
///
/// This is the single shared implementation behind both the standalone
/// `settle_funding` crank AND the inline settle-before-resize in `apply_fill` /
/// `apply_flp_fill` (R2): settling on the pre-trade size before a same-side add
/// stops the post-add size being charged funding for the whole prior interval
/// (the phantom-funding bug). Callers do the account binding/validation; this is
/// the pure money math (host-testable).
#[allow(clippy::result_unit_err)]
pub fn settle_position_funding(
    position: &mut crate::state::Position,
    mark_price_ticks: u64,
    tick_size: u64,
    cum_now: i128,
    ts_collateral: &mut u64,
    residual: &mut u128,
) -> Result<(), FundingSettleErr> {
    if position.size_lots == 0 {
        position.set_cum_funding(cum_now);
        return Ok(());
    }
    let notional = (position.size_lots as u128)
        .checked_mul(mark_price_ticks as u128).ok_or(FundingSettleErr::Overflow)?
        .checked_mul(tick_size as u128).ok_or(FundingSettleErr::Overflow)?;
    if notional > u64::MAX as u128 {
        return Err(FundingSettleErr::Overflow);
    }
    let owed = funding_owed(position.side == 0, notional as u64, cum_now, position.cum_funding())
        .ok_or(FundingSettleErr::Overflow)?;
    // Clamp to i64 range (only reachable with insane rates) — matches the crank.
    let owed_i64: i64 = if owed > i64::MAX as i128 { i64::MAX }
        else if owed < i64::MIN as i128 { i64::MIN }
        else { owed as i64 };

    let is_isolated = position.collateral_quote_lots > 0;
    let mut paid: u64 = 0;
    let mut received: u64 = 0;
    if owed_i64 > 0 {
        let owed_u64 = owed_i64 as u64; // PAYS — clamp to availability
        if is_isolated {
            paid = owed_u64.min(position.collateral_quote_lots);
            position.collateral_quote_lots -= paid;
        } else {
            paid = owed_u64.min(*ts_collateral);
            *ts_collateral -= paid;
        }
    } else if owed_i64 < 0 {
        received = owed_i64.unsigned_abs(); // RECEIVES — credit in full
        if is_isolated {
            position.collateral_quote_lots = position.collateral_quote_lots
                .checked_add(received).ok_or(FundingSettleErr::Overflow)?;
        } else {
            *ts_collateral = ts_collateral.checked_add(received).ok_or(FundingSettleErr::Overflow)?;
        }
    }
    // RISK-1: residual ↑ when the trader pays, ↓ when it receives (underflow ⇒
    // insolvency ⇒ reject). Δcollateral == −Δresidual.
    if paid > 0 || received > 0 {
        let delta: i128 = paid as i128 - received as i128;
        *residual = crate::haircut::apply_residual_delta(*residual, delta)
            .map_err(|_| FundingSettleErr::ResidualUnderflow)?;
    }
    position.set_cum_funding(cum_now);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const Q: i128 = 1 << 64;
    #[test] fn long_pays_when_index_rises() { // delta = +1.0 index unit, notional 1000
        assert_eq!(funding_owed(true, 1000, Q, 0), Some(1000)); }
    #[test] fn short_receives_when_index_rises() {
        assert_eq!(funding_owed(false, 1000, Q, 0), Some(-1000)); }
    #[test] fn zero_delta() { assert_eq!(funding_owed(true, 1_000_000, 5*Q, 5*Q), Some(0)); }
    #[test] fn fractional_floor() { // delta = 0.5 index, notional 10 -> 5
        assert_eq!(funding_owed(true, 10, Q/2, 0), Some(5)); }
    #[test] fn negative_index_move_long_receives() { // delta = -1.0 -> long receives
        assert_eq!(funding_owed(true, 1000, 0, Q), Some(-1000)); }

    // ── R2 shared settle helper: bucket routing + residual + re-stamp ────────
    fn pos(side: u8, size: u64, collat: u64) -> crate::state::Position {
        crate::state::Position {
            disc: [0; 8], cum_funding_index: [0; 16], trader: [0; 32], market: [0; 32],
            size_lots: size, entry_price_ticks: 0, collateral_quote_lots: collat,
            realized_pnl_quote_lots: 0, side, sub_index: 0, _pad0: [0; 2], leverage_cap: 0,
        }
    }
    #[test]
    fn settle_position_funding_routes_and_restamps() {
        // notional = 10·1000·1 = 10_000; index rose +1.0 ⇒ |owed| = 10_000.
        // CROSS LONG pays from the trader pool; residual ↑; entry index re-stamped.
        let mut p = pos(0, 10, 0);
        let (mut ts, mut res) = (50_000u64, 100_000u128);
        settle_position_funding(&mut p, 1000, 1, Q, &mut ts, &mut res).unwrap();
        assert_eq!((ts, res, p.cum_funding()), (40_000, 110_000, Q));
        // idempotent: re-settling at the same index owes 0, changes nothing.
        settle_position_funding(&mut p, 1000, 1, Q, &mut ts, &mut res).unwrap();
        assert_eq!((ts, res), (40_000, 110_000));

        // CROSS SHORT receives; residual ↓.
        let mut s = pos(1, 10, 0);
        let (mut ts2, mut res2) = (0u64, 100_000u128);
        settle_position_funding(&mut s, 1000, 1, Q, &mut ts2, &mut res2).unwrap();
        assert_eq!((ts2, res2), (10_000, 90_000));

        // ISOLATED long pays from the POSITION's own bucket; the cross pool is untouched.
        let mut i = pos(0, 10, 50_000);
        let (mut ts3, mut res3) = (777u64, 100_000u128);
        settle_position_funding(&mut i, 1000, 1, Q, &mut ts3, &mut res3).unwrap();
        assert_eq!((i.collateral_quote_lots, ts3, res3), (40_000, 777, 110_000));

        // A pay is CLAMPED to availability (never underflows the bucket).
        let mut po = pos(0, 10, 0);
        let (mut ts4, mut res4) = (3_000u64, 100_000u128); // owes 10_000, only 3_000 available
        settle_position_funding(&mut po, 1000, 1, Q, &mut ts4, &mut res4).unwrap();
        assert_eq!((ts4, res4), (0, 103_000)); // paid only 3_000

        // A receive that would drive the residual negative ⇒ insolvency error.
        let mut sr = pos(1, 10, 0);
        let (mut ts5, mut res5) = (0u64, 5_000u128); // would need residual −= 10_000
        assert_eq!(
            settle_position_funding(&mut sr, 1000, 1, Q, &mut ts5, &mut res5),
            Err(FundingSettleErr::ResidualUnderflow)
        );

        // A flat position just re-stamps and owes nothing.
        let mut f = pos(0, 0, 0);
        let (mut ts6, mut res6) = (1u64, 1u128);
        settle_position_funding(&mut f, 1000, 1, Q, &mut ts6, &mut res6).unwrap();
        assert_eq!((ts6, res6, f.cum_funding()), (1, 1, Q));
    }
}
