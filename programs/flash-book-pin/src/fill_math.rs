//! Pure fill-settlement math — a faithful transcription of the Anchor
//! `apply_fill_to_position` (programs/flash-book/src/lib.rs). No pinocchio,
//! no syscalls → host-unit-testable for correctness equivalence.
use crate::state::Position;

#[derive(Debug, PartialEq, Eq)]
pub enum FillErr { Overflow }

/// Apply one fill leg to a position: open / weighted-avg increase / realize
/// PnL on close / side flip. Identical semantics to the Anchor version.
pub fn apply_to_position(pos: &mut Position, fill_side: u8, fill_size: u64, fill_price: u64, fidx: i128) -> Result<(), FillErr> {
    if pos.size_lots == 0 {
        pos.side = fill_side;
        pos.size_lots = fill_size;
        pos.entry_price_ticks = fill_price;
        pos.set_cum_funding(fidx);
        return Ok(());
    }
    if pos.side == fill_side {
        let new_size = pos.size_lots.checked_add(fill_size).ok_or(FillErr::Overflow)?;
        let weighted = (pos.entry_price_ticks as u128)
            .checked_mul(pos.size_lots as u128).ok_or(FillErr::Overflow)?
            .checked_add((fill_price as u128).checked_mul(fill_size as u128).ok_or(FillErr::Overflow)?)
            .ok_or(FillErr::Overflow)?
            / new_size as u128;
        pos.entry_price_ticks = weighted as u64;
        pos.size_lots = new_size;
        return Ok(());
    }
    let close = fill_size.min(pos.size_lots);
    let sign: i128 = if pos.side == 0 { 1 } else { -1 };
    let pnl = sign
        .checked_mul(close as i128).ok_or(FillErr::Overflow)?
        .checked_mul((fill_price as i128) - (pos.entry_price_ticks as i128)).ok_or(FillErr::Overflow)?;
    let pnl_c = if pnl > i64::MAX as i128 { i64::MAX } else if pnl < i64::MIN as i128 { i64::MIN } else { pnl as i64 };
    pos.realized_pnl_quote_lots = pos.realized_pnl_quote_lots.checked_add(pnl_c).ok_or(FillErr::Overflow)?;
    if fill_size <= pos.size_lots {
        pos.size_lots -= fill_size;
        if pos.size_lots == 0 { pos.entry_price_ticks = 0; pos.set_cum_funding(fidx); }
    } else {
        pos.side = fill_side;
        pos.size_lots = fill_size - pos.size_lots;
        pos.entry_price_ticks = fill_price;
        pos.set_cum_funding(fidx);
    }
    Ok(())
}

/// Update `(long_oi, short_oi)` for ONE position leg transitioning from
/// `(old_side, old_size)` to `(new_side, new_size)`. Each position contributes
/// its `size` to the open interest on its side; we remove the old contribution
/// and add the new. `side`: 0 = long, 1 = short. Pure + host-tested. Applied to
/// both legs of a fill, it keeps `long_oi == short_oi` (the conservation
/// invariant), since each fill changes the two legs by mirror amounts.
#[inline]
pub fn oi_after_leg(
    mut long_oi: u64,
    mut short_oi: u64,
    old_side: u8,
    old_size: u64,
    new_side: u8,
    new_size: u64,
) -> (u64, u64) {
    if old_size > 0 {
        if old_side == 0 {
            long_oi = long_oi.saturating_sub(old_size);
        } else {
            short_oi = short_oi.saturating_sub(old_size);
        }
    }
    if new_size > 0 {
        if new_side == 0 {
            long_oi = long_oi.saturating_add(new_size);
        } else {
            short_oi = short_oi.saturating_add(new_size);
        }
    }
    (long_oi, short_oi)
}

/// Open-interest update for an `apply_flp_fill` settlement, where the **FLP
/// pool is the maker** and has no on-chain `Position` (its per-market exposure
/// side/size is tracked elsewhere / deferred). The taker leg is precise
/// (`oi_after_leg`); the pool, being the taker's exact counterparty for this
/// fill, contributes the MIRROR of the taker's per-side deltas — adding the
/// taker's short-delta to `long_oi` and its long-delta to `short_oi`. This
/// keeps `long_oi == short_oi` (the conservation invariant) across open / close
/// / flip without needing the pool's tracked position. Pure + host-tested.
///
/// (The prior code added the fill size to ONLY the taker side, breaking the
/// invariant on every FLP fill — `verify_market_invariants` would auto-pause
/// the market.)
#[inline]
pub fn oi_after_flp_fill(
    long_oi: u64,
    short_oi: u64,
    t_old_side: u8,
    t_old_size: u64,
    t_new_side: u8,
    t_new_size: u64,
) -> (u64, u64) {
    let (l1, s1) = oi_after_leg(long_oi, short_oi, t_old_side, t_old_size, t_new_side, t_new_size);
    // Taker's per-side deltas.
    let dl = l1 as i128 - long_oi as i128;
    let ds = s1 as i128 - short_oi as i128;
    // Pool (maker) mirror: +ds to long, +dl to short. Total long-delta = dl+ds
    // = total short-delta, so equality is preserved.
    let new_long = (l1 as i128 + ds).max(0) as u64;
    let new_short = (s1 as i128 + dl).max(0) as u64;
    (new_long, new_short)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FLP fill: taker leg + pool mirror must preserve long_oi == short_oi.
    #[test]
    fn flp_oi_conserved() {
        // Taker opens long 10 from a flat book; pool (maker) takes short 10.
        assert_eq!(oi_after_flp_fill(0, 0, 0, 0, 0, 10), (10, 10));
        // Taker opens short 10; pool takes long 10.
        assert_eq!(oi_after_flp_fill(0, 0, 1, 0, 1, 10), (10, 10));
        // Taker closes long 10 → 0; pool closes its short.
        assert_eq!(oi_after_flp_fill(10, 10, 0, 10, 0, 0), (0, 0));
        // Taker partial reduce long 10 → 4; pool reduces short to 4.
        assert_eq!(oi_after_flp_fill(10, 10, 0, 10, 0, 4), (4, 4));
        // Taker flips short 5 → long 5; pool flips long 5 → short 5. Unchanged.
        assert_eq!(oi_after_flp_fill(5, 5, 1, 5, 0, 5), (5, 5));
    }

    /// Apply a balanced fill (taker on `t_side`, maker on `1-t_side`, both by
    /// `size`) to both legs and assert long_oi == short_oi is preserved.
    fn fill_oi(long: u64, short: u64, legs: &[(u8, u64, u8, u64)]) -> (u64, u64) {
        let mut l = long;
        let mut s = short;
        for &(os, oz, ns, nz) in legs {
            let (nl, ns2) = oi_after_leg(l, s, os, oz, ns, nz);
            l = nl;
            s = ns2;
        }
        (l, s)
    }

    #[test]
    fn oi_conserved_open_close_flip() {
        // Opening fill from a flat book: taker opens long 10, maker opens short 10.
        let (l, s) = fill_oi(0, 0, &[(0, 0, 0, 10), (0, 0, 1, 10)]);
        assert_eq!((l, s), (10, 10));
        // Closing fill: taker (long 10 → 0), maker (short 10 → 0).
        let (l, s) = fill_oi(10, 10, &[(0, 10, 0, 0), (1, 10, 1, 0)]);
        assert_eq!((l, s), (0, 0));
        // Partial reduce: taker (long 10 → 4), maker (short 10 → 4).
        let (l, s) = fill_oi(10, 10, &[(0, 10, 0, 4), (1, 10, 1, 4)]);
        assert_eq!((l, s), (4, 4));
        // Flip: taker (short 5 → long 5), maker (long 5 → short 5).
        let (l, s) = fill_oi(5, 5, &[(1, 5, 0, 5), (0, 5, 1, 5)]);
        assert_eq!((l, s), (5, 5));
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;
    fn p(side: u8, size: u64, entry: u64) -> Position {
        Position { disc:[0;8], cum_funding_index:[0;16], trader:[0;32], market:[0;32],
            size_lots:size, entry_price_ticks:entry, collateral_quote_lots:0, realized_pnl_quote_lots:0, side, _pad0:[0;3], leverage_cap:0 }
    }
    #[test] fn open() { let mut x=p(0,0,0); apply_to_position(&mut x,1,10,200,7).unwrap();
        assert_eq!((x.side,x.size_lots,x.entry_price_ticks,x.cum_funding()),(1,10,200,7)); }
    #[test] fn increase_weighted_entry() { let mut x=p(0,10,100); apply_to_position(&mut x,0,10,200,0).unwrap();
        assert_eq!((x.size_lots,x.entry_price_ticks),(20,150)); }
    #[test] fn partial_close_long() { let mut x=p(0,10,100); apply_to_position(&mut x,1,4,150,0).unwrap();
        assert_eq!((x.size_lots,x.entry_price_ticks,x.realized_pnl_quote_lots),(6,100,200)); }
    #[test] fn full_close_long() { let mut x=p(0,10,100); apply_to_position(&mut x,1,10,150,9).unwrap();
        assert_eq!((x.size_lots,x.entry_price_ticks,x.realized_pnl_quote_lots,x.cum_funding()),(0,0,500,9)); }
    #[test] fn flip_long_to_short() { let mut x=p(0,10,100); apply_to_position(&mut x,1,15,150,3).unwrap();
        assert_eq!((x.side,x.size_lots,x.entry_price_ticks,x.realized_pnl_quote_lots,x.cum_funding()),(1,5,150,500,3)); }
    #[test] fn short_realizes_on_drop() { let mut x=p(1,10,100); apply_to_position(&mut x,0,10,80,0).unwrap();
        assert_eq!((x.size_lots,x.realized_pnl_quote_lots),(0,200)); }
}
