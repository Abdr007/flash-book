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

// ── Kani proofs: open-interest conservation ──────────────────────────────
// The matcher invariant `verify_market_invariants` enforces is `long_oi ==
// short_oi` (every long lot is matched by a short). These harnesses prove that
// a single balanced fill — the apply_fill two-leg case and the apply_flp_fill
// FLP-maker case — changes `long_oi` and `short_oi` by the SAME amount, so the
// equality is preserved inductively. All values are bounded to `u32` (cast to
// u64) so there is no `u128` symbolic mul/div (which Kani cannot terminate on)
// and the `saturating_*` / `.max(0)` clamps provably never fire under the
// realistic precondition that a position's old size is part of its side's OI.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Size/side-only transition mirroring `apply_to_position` (no price/PnL):
    /// open from flat / add same-side / reduce-or-close / flip. This is exactly
    /// the OI-relevant part of a fill leg.
    fn leg_transition(old_side: u8, old_size: u64, fill_side: u8, fill_size: u64) -> (u8, u64) {
        if old_size == 0 {
            (fill_side, fill_size)
        } else if old_side == fill_side {
            (old_side, old_size + fill_size)
        } else if fill_size <= old_size {
            (old_side, old_size - fill_size) // reduce (==0 ⇒ size 0, side irrelevant)
        } else {
            (fill_side, fill_size - old_size) // flip
        }
    }

    /// apply_fill: a balanced fill (taker on `taker_side` + maker on the
    /// opposite side, both by `size`) changes long_oi and short_oi equally, so
    /// `long_oi == short_oi` is preserved.
    #[kani::proof]
    fn proof_fill_two_leg_oi_balanced() {
        let long = kani::any::<u32>() as u64;
        let short = kani::any::<u32>() as u64;
        let size = kani::any::<u32>() as u64;
        kani::assume(size > 0);
        let taker_side = kani::any::<u8>();
        kani::assume(taker_side <= 1);
        let maker_side = 1 - taker_side;
        let t_old_side = kani::any::<u8>();
        kani::assume(t_old_side <= 1);
        let m_old_side = kani::any::<u8>();
        kani::assume(m_old_side <= 1);
        let t_old_size = kani::any::<u32>() as u64;
        let m_old_size = kani::any::<u32>() as u64;
        // Precondition: each leg's OLD size is part of its side's OI, so removal
        // never saturates. Account for both legs sharing a side.
        let long_used =
            (if t_old_side == 0 { t_old_size } else { 0 }) + (if m_old_side == 0 { m_old_size } else { 0 });
        let short_used =
            (if t_old_side == 1 { t_old_size } else { 0 }) + (if m_old_side == 1 { m_old_size } else { 0 });
        kani::assume(long >= long_used);
        kani::assume(short >= short_used);

        let (t_ns, t_nz) = leg_transition(t_old_side, t_old_size, taker_side, size);
        let (m_ns, m_nz) = leg_transition(m_old_side, m_old_size, maker_side, size);
        let (l1, s1) = oi_after_leg(long, short, t_old_side, t_old_size, t_ns, t_nz);
        let (l2, s2) = oi_after_leg(l1, s1, m_old_side, m_old_size, m_ns, m_nz);
        // Δlong == Δshort ⇒ equality preserved (if long==short before, after too).
        assert_eq!(l2 as i128 - long as i128, s2 as i128 - short as i128);
    }

    /// apply_flp_fill: starting from the conservation invariant `long == short`,
    /// the taker leg plus the pool's mirror counter-leg keep `long_oi ==
    /// short_oi`. (The mirror gives both sides the same raw value `l1 + s1 −
    /// long`, so equality holds whether or not the i128→u64 `.max(0)` clamp
    /// fires — the precondition is the invariant the matcher maintains.)
    #[kani::proof]
    fn proof_flp_fill_oi_balanced() {
        let long = kani::any::<u32>() as u64;
        let short = kani::any::<u32>() as u64;
        kani::assume(long == short); // invariant holds before the fill
        let size = kani::any::<u32>() as u64;
        kani::assume(size > 0);
        let taker_side = kani::any::<u8>();
        kani::assume(taker_side <= 1);
        let t_old_side = kani::any::<u8>();
        kani::assume(t_old_side <= 1);
        let t_old_size = kani::any::<u32>() as u64;
        // The taker's old size is part of its side's OI (removal never saturates).
        kani::assume(if t_old_side == 0 { t_old_size <= long } else { t_old_size <= short });

        let (t_ns, t_nz) = leg_transition(t_old_side, t_old_size, taker_side, size);
        let (nl, ns) = oi_after_flp_fill(long, short, t_old_side, t_old_size, t_ns, t_nz);
        assert_eq!(nl, ns); // invariant preserved
    }
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
            size_lots:size, entry_price_ticks:entry, collateral_quote_lots:0, realized_pnl_quote_lots:0, side, sub_index:0, _pad0:[0;2], leverage_cap:0 }
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

    /// State-contract fuzz for the core settlement fn: drive 20k random fills
    /// through ONE position and assert the invariants `apply_fill` and the OI /
    /// margin code rely on after EVERY fill: `side ∈ {0,1}`; a flat position
    /// (size 0) carries zero entry; opening from flat takes the fill's
    /// side/size/price exactly; a same-side ADD's weighted entry never escapes
    /// `[min, max]` of the prior entry and the fill price (no weighted-avg
    /// blow-up / overflow). Deterministic LCG ⇒ reproducible.
    #[test]
    fn apply_to_position_state_invariants() {
        let mut seed: u64 = 0xCAFE_F00D_D15E_A5E5;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        let mut x = p(0, 0, 0);
        for _ in 0..20_000 {
            let fill_side = (next() % 2) as u8;
            let fill_size = 1 + next() % 100;
            let fill_price = 1 + next() % 10_000;
            let (old_side, old_size, old_entry) = (x.side, x.size_lots, x.entry_price_ticks);
            apply_to_position(&mut x, fill_side, fill_size, fill_price, 0).unwrap();

            assert!(x.side <= 1, "side out of range");
            if x.size_lots == 0 {
                assert_eq!(x.entry_price_ticks, 0, "flat position must have zero entry");
            }
            if old_size == 0 {
                assert_eq!(
                    (x.side, x.size_lots, x.entry_price_ticks),
                    (fill_side, fill_size, fill_price),
                    "open-from-flat must take the fill exactly"
                );
            } else if old_side == fill_side {
                let lo = old_entry.min(fill_price);
                let hi = old_entry.max(fill_price);
                assert!(
                    x.entry_price_ticks >= lo && x.entry_price_ticks <= hi,
                    "same-side weighted entry {} escaped [{lo}, {hi}]",
                    x.entry_price_ticks
                );
            }
        }
    }

    /// Integration-level open-interest conservation: drive thousands of random
    /// BALANCED fills (taker on one side, maker on the opposite, same size)
    /// through the REAL `apply_to_position` + `oi_after_leg` path — the exact
    /// settlement code `apply_fill` runs — over a small book of traders that
    /// open / add / partially close / fully close / flip. After every fill it
    /// asserts (1) `long_oi == short_oi` (the matcher conservation invariant)
    /// and (2) the incrementally-tracked OI equals a from-scratch recompute of
    /// every position's contribution. Complements the single-fill Kani proofs
    /// with sequence-level coverage. Deterministic LCG ⇒ reproducible.
    #[test]
    fn oi_conservation_random_fill_sequences() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        const N: usize = 6;
        let mut book: Vec<Position> = (0..N).map(|_| p(0, 0, 0)).collect();
        let mut long_oi: u64 = 0;
        let mut short_oi: u64 = 0;
        for _ in 0..5000 {
            let t = (next() as usize) % N;
            let mut m = (next() as usize) % N;
            if m == t {
                m = (m + 1) % N;
            }
            let taker_side = (next() % 2) as u8;
            let maker_side = 1 - taker_side;
            let size = 1 + next() % 50;
            let price = 100 + next() % 100;

            // Taker leg (snapshot old side/size first, exactly like apply_fill).
            let (t_os, t_oz) = (book[t].side, book[t].size_lots);
            apply_to_position(&mut book[t], taker_side, size, price, 0).unwrap();
            let (l, s) =
                oi_after_leg(long_oi, short_oi, t_os, t_oz, book[t].side, book[t].size_lots);
            long_oi = l;
            short_oi = s;
            // Maker leg — opposite side, same size.
            let (m_os, m_oz) = (book[m].side, book[m].size_lots);
            apply_to_position(&mut book[m], maker_side, size, price, 0).unwrap();
            let (l, s) =
                oi_after_leg(long_oi, short_oi, m_os, m_oz, book[m].side, book[m].size_lots);
            long_oi = l;
            short_oi = s;

            // (1) conservation invariant.
            assert_eq!(long_oi, short_oi, "long_oi != short_oi after a balanced fill");
            // (2) incremental OI must equal a full recompute from the book.
            let mut rl = 0u64;
            let mut rs = 0u64;
            for q in &book {
                if q.size_lots > 0 {
                    if q.side == 0 {
                        rl += q.size_lots;
                    } else {
                        rs += q.size_lots;
                    }
                }
            }
            assert_eq!((long_oi, short_oi), (rl, rs), "incremental OI != recompute");
        }
    }
}
