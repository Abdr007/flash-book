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
}
