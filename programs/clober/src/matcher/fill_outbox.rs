//! Fill-Outbox — on-chain fill-DATA mirror (lifts the per-tx batch cap past the
//! program-log ceiling). See `docs/SETTLEMENT.md` for the full rationale.
//!
//! ## Why this exists
//! `place_taker_order` ships each crossed fill's economic data to the off-chain
//! sequencer via the `FillBatchEvent` log. Solana caps *all* program-log output at
//! `LOG_MESSAGES_BYTES_LIMIT = 10_000` bytes/tx and base64-inflates `sol_log_data`,
//! so one event overflows ~125 fills and silently truncates the tail — and a
//! truncated-but-crossed fill is unsettleable (its commitment-ring slot never
//! pops), wedging settlement. That pins the matcher batch cap at 96.
//!
//! The outbox removes the log from the fill-DATA path: the matcher writes each
//! fill's data into a persistent per-market PDA the sequencer reads with
//! `getAccountInfo`. Accounts hold up to 10 MB (no log limit), so the cap can rise
//! to 256.
//!
//! ## Design — additive data mirror over the proven commitment ring
//! This is NOT a second cursor. The outbox is a parallel data array **addressed by
//! the existing `fill_commitment` ring's `produced` cursor**: the matcher pushes a
//! keccak commitment to the ring (which decides slot `produced % cap`) and writes
//! the same fill's data to `outbox[produced % cap]` in the same loop. `apply_fill`
//! and the Kani-proven ring state machine are UNTOUCHED — the outbox is
//! write-only-by-matcher, read-only-by-sequencer, and adds no authenticity surface
//! (the keccak ring stays the sole trust anchor).
//!
//! ## The critical invariant — no silent overwrite (OpenBook-class backpressure)
//! `outbox.cap == ring.cap` ALWAYS (both 256). The ring's `ring_push` returns
//! `Full` when `depth = produced − settled >= cap`, which fails the matcher tx — so
//! `produced` can never advance past `settled + cap`. Therefore slot `i % cap` is
//! overwritten only at `produced = i`, whose previous occupant was written at
//! `produced = i − cap < settled` — i.e. already settled and consumed. A slow
//! sequencer is given hard backpressure (the taker's tx reverts `FillRingFull`)
//! rather than being silently lapped. This is exactly OpenBook v2's
//! `assert!(!is_full())` posture, inherited for free from the ring.
//!
//! ## Account layout (raw PDA `[fill_outbox, market]`)
//! Fixed 64-byte header (mirrors the ring header so the ER delegate/commit code is
//! a copy-adaptation) + `cap` fixed-width 96-byte data slots:
//!
//! | header off | len | field |
//! |-----------:|----:|-------|
//! |          0 |   8 | disc `FBoutbx\0` |
//! |          8 |   8 | `produced` (mirror of ring, matcher-written) |
//! |         16 |   8 | `settled`  (mirror of ring, matcher-written) |
//! |         24 |   4 | `cap` (u32) — MUST equal the ring cap |
//! |         28 |   1 | bump |
//! |         29 |   3 | pad |
//! |         32 |  32 | `market` |
//!
//! | slot off | len | field |
//! |---------:|----:|-------|
//! |        0 |  32 | `taker` |
//! |       32 |  32 | `maker` (`Pubkey::default()` ⇒ LP virtual-quote fill) |
//! |       64 |   8 | `size_lots` |
//! |       72 |   8 | `price_ticks` |
//! |       80 |   8 | `maker_id` (resting order id — sequencer bookkeeping) |
//! |       88 |   1 | `taker_side` |
//! |       89 |   1 | `taker_sub_index` |
//! |       90 |   1 | `maker_sub_index` |
//! |       91 |   1 | `taker_was_jit` |
//! |       92 |   4 | pad → 96 B/slot (8-byte aligned) |
//!
//! These are exactly the fields `apply_fill` consumes plus `maker_id` (parity with
//! `FillBatchEvent`); the fill's global sequence number is implicit in its absolute
//! `produced` index, so the reader needs no per-slot seq field.

/// PDA seed for the per-market `FillOutboxAccount`: `[FILL_OUTBOX_SEED, market]`.
pub const FILL_OUTBOX_SEED: &[u8] = b"fill_outbox";

/// 8-byte discriminator marking a raw account as a FillOutboxAccount. Distinct
/// from the ring's `FBfcq…` disc so the two PDAs can never be confused.
pub const FILL_OUTBOX_DISC: [u8; 8] = *b"FBoutbx\x00";

/// Fixed header length (disc + counters + cap + bump + pad + market pubkey).
pub const FILL_OUTBOX_HEADER_LEN: usize = 64;

/// One fill's data, fixed width (8-byte aligned).
pub const FILL_OUTBOX_SLOT_LEN: usize = 96;

// Header byte offsets.
const OFF_PRODUCED: usize = 8; // u64 LE
const OFF_SETTLED: usize = 16; // u64 LE
const OFF_CAP: usize = 24; // u32 LE
const OFF_BUMP: usize = 28; // u8
const OFF_MARKET: usize = 32; // [u8; 32]

// Slot byte offsets (relative to slot start).
const SL_TAKER: usize = 0; // [u8; 32]
const SL_MAKER: usize = 32; // [u8; 32]
const SL_SIZE: usize = 64; // u64 LE
const SL_PRICE: usize = 72; // u64 LE
const SL_MAKER_ID: usize = 80; // u64 LE
const SL_TAKER_SIDE: usize = 88; // u8
const SL_TAKER_SUB: usize = 89; // u8
const SL_MAKER_SUB: usize = 90; // u8
const SL_JIT: usize = 91; // u8
                          // bytes 92..96 zero pad

/// Pure errors mirroring the ring's; the handler maps onto `CloberError`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillOutboxError {
    /// Account too small, wrong disc, or length not header+cap*slot.
    Corrupt,
    /// Bound market does not match the expected market pubkey (tamper / wrong PDA).
    WrongMarket,
    /// A slot index outside `0..cap` was requested (caller bug — never on the
    /// honest `produced % cap` path, but checked so a tampered cap fails closed).
    OutOfRange,
}

/// Total account size for a given slot capacity.
pub const fn fill_outbox_account_len(cap: usize) -> usize {
    FILL_OUTBOX_HEADER_LEN + cap * FILL_OUTBOX_SLOT_LEN
}

/// Capacity the outbox is CREATED at by `init_fill_outbox`. A program CPI to
/// `create_account` can grow an account by at most `MAX_PERMITTED_DATA_INCREASE`
/// (10,240 B) in one instruction, so a full 256-slot outbox (24,640 B) cannot be
/// allocated in a single ix — exactly why the market book (9,600 B) and any large
/// PDA here are created small and grown. The outbox follows the same lifecycle:
/// `init_fill_outbox` allocates `FILL_OUTBOX_INIT_CAP` slots, then
/// `grow_fill_outbox` raises it (≤106 slots/call) up to the ring capacity. The
/// matcher's `outbox.cap >= ring.cap` guard keeps outbox-armed matching INERT until
/// the outbox has been grown to cover the ring — fail-closed, never a silent
/// overwrite. `64 + 105*96 = 10,144 B`, safely under the 10,240 limit.
pub const FILL_OUTBOX_INIT_CAP: u32 = 105;

#[inline]
fn rd_u32(data: &[u8], off: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&data[off..off + 4]);
    u32::from_le_bytes(b)
}
#[inline]
fn wr_u64(data: &mut [u8], off: usize, v: u64) {
    data[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// One-time initialise a freshly-allocated, zeroed account buffer: stamp disc,
/// market, cap, bump. `data.len()` must equal `fill_outbox_account_len(cap)`.
pub fn outbox_init(
    data: &mut [u8],
    market: &[u8; 32],
    cap: u32,
    bump: u8,
) -> Result<(), FillOutboxError> {
    if cap == 0 || data.len() != fill_outbox_account_len(cap as usize) {
        return Err(FillOutboxError::Corrupt);
    }
    data[0..8].copy_from_slice(&FILL_OUTBOX_DISC);
    wr_u64(data, OFF_PRODUCED, 0);
    wr_u64(data, OFF_SETTLED, 0);
    data[OFF_CAP..OFF_CAP + 4].copy_from_slice(&cap.to_le_bytes());
    data[OFF_BUMP] = bump;
    data[OFF_BUMP + 1..OFF_MARKET].fill(0); // pad
    data[OFF_MARKET..OFF_MARKET + 32].copy_from_slice(market);
    Ok(())
}

/// Validate a raw account buffer: disc, market binding, and that the length is
/// exactly header + cap*slot for the stored cap. Returns the cap. Fails CLOSED on
/// any inconsistency (inherits the ER hardening posture).
pub fn outbox_check(data: &[u8], expected_market: &[u8; 32]) -> Result<u32, FillOutboxError> {
    if data.len() < FILL_OUTBOX_HEADER_LEN || data[0..8] != FILL_OUTBOX_DISC {
        return Err(FillOutboxError::Corrupt);
    }
    let cap = rd_u32(data, OFF_CAP);
    if cap == 0 || data.len() != fill_outbox_account_len(cap as usize) {
        return Err(FillOutboxError::Corrupt);
    }
    if data[OFF_MARKET..OFF_MARKET + 32] != expected_market[..] {
        return Err(FillOutboxError::WrongMarket);
    }
    Ok(cap)
}

/// Set the stored cap (used by `grow_fill_outbox` after a realloc, kept in lockstep
/// with the ring). The caller guarantees the account was already resized to
/// `fill_outbox_account_len(new_cap)` and the new tail is zeroed.
pub fn outbox_set_cap(data: &mut [u8], new_cap: u32) {
    data[OFF_CAP..OFF_CAP + 4].copy_from_slice(&new_cap.to_le_bytes());
}

/// Mirror the ring's `(produced, settled)` cursors into the outbox header. Called
/// by the matcher once per batch (it reads both off the ring anyway). Gives the
/// sequencer stream semantics — read slots `[last_seen_produced .. produced)` — and
/// a recent `settled` low-water mark for gap detection, without coupling
/// `apply_fill` to this account.
pub fn outbox_set_cursors(data: &mut [u8], produced: u64, settled: u64) {
    wr_u64(data, OFF_PRODUCED, produced);
    wr_u64(data, OFF_SETTLED, settled);
}

/// Read back the mirrored `produced` cursor (the fill high-water mark).
#[inline]
pub fn outbox_produced(data: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[OFF_PRODUCED..OFF_PRODUCED + 8]);
    u64::from_le_bytes(b)
}

/// Read back the mirrored `settled` cursor. Used by `grow_fill_outbox` to enforce
/// the drained invariant (a non-drained grow would remap every slot's `idx % cap`
/// position and misread pending fills), mirroring `grow_fill_commitment`.
#[inline]
pub fn outbox_settled(data: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[OFF_SETTLED..OFF_SETTLED + 8]);
    u64::from_le_bytes(b)
}

/// Write one fill's data into slot `slot_index` (`= produced % cap`). Heap-free —
/// a handful of `copy_from_slice`s into the borrowed account window, no `Vec` and
/// no serialization. `slot_index` MUST be `< cap` (enforced — a tampered cap that
/// made `produced % cap` exceed the real slot region fails closed rather than
/// writing out of bounds).
#[allow(clippy::too_many_arguments)]
pub fn outbox_write_slot(
    data: &mut [u8],
    cap: u32,
    slot_index: u64,
    taker: &[u8; 32],
    maker: &[u8; 32],
    size_lots: u64,
    price_ticks: u64,
    maker_id: u64,
    taker_side: u8,
    taker_sub_index: u8,
    maker_sub_index: u8,
    taker_was_jit: u8,
) -> Result<(), FillOutboxError> {
    if slot_index >= cap as u64 {
        return Err(FillOutboxError::OutOfRange);
    }
    let base = FILL_OUTBOX_HEADER_LEN + (slot_index as usize) * FILL_OUTBOX_SLOT_LEN;
    if base + FILL_OUTBOX_SLOT_LEN > data.len() {
        return Err(FillOutboxError::Corrupt);
    }
    let s = &mut data[base..base + FILL_OUTBOX_SLOT_LEN];
    s[SL_TAKER..SL_TAKER + 32].copy_from_slice(taker);
    s[SL_MAKER..SL_MAKER + 32].copy_from_slice(maker);
    s[SL_SIZE..SL_SIZE + 8].copy_from_slice(&size_lots.to_le_bytes());
    s[SL_PRICE..SL_PRICE + 8].copy_from_slice(&price_ticks.to_le_bytes());
    s[SL_MAKER_ID..SL_MAKER_ID + 8].copy_from_slice(&maker_id.to_le_bytes());
    s[SL_TAKER_SIDE] = taker_side;
    s[SL_TAKER_SUB] = taker_sub_index;
    s[SL_MAKER_SUB] = maker_sub_index;
    s[SL_JIT] = taker_was_jit;
    s[92..96].fill(0); // pad
    Ok(())
}

/// A decoded outbox slot — used by host tests (and available to off-chain Rust
/// consumers) to read a fill back. On-chain settlement never decodes this (the
/// outbox is sequencer transport only).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutboxSlot {
    pub taker: [u8; 32],
    pub maker: [u8; 32],
    pub size_lots: u64,
    pub price_ticks: u64,
    pub maker_id: u64,
    pub taker_side: u8,
    pub taker_sub_index: u8,
    pub maker_sub_index: u8,
    pub taker_was_jit: u8,
}

/// Decode slot `slot_index`. Bounds-checked; `OutOfRange` past `cap`.
pub fn outbox_read_slot(
    data: &[u8],
    cap: u32,
    slot_index: u64,
) -> Result<OutboxSlot, FillOutboxError> {
    if slot_index >= cap as u64 {
        return Err(FillOutboxError::OutOfRange);
    }
    let base = FILL_OUTBOX_HEADER_LEN + (slot_index as usize) * FILL_OUTBOX_SLOT_LEN;
    if base + FILL_OUTBOX_SLOT_LEN > data.len() {
        return Err(FillOutboxError::Corrupt);
    }
    let s = &data[base..base + FILL_OUTBOX_SLOT_LEN];
    let mut taker = [0u8; 32];
    taker.copy_from_slice(&s[SL_TAKER..SL_TAKER + 32]);
    let mut maker = [0u8; 32];
    maker.copy_from_slice(&s[SL_MAKER..SL_MAKER + 32]);
    let rd8 = |o: usize| -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&s[o..o + 8]);
        u64::from_le_bytes(b)
    };
    Ok(OutboxSlot {
        taker,
        maker,
        size_lots: rd8(SL_SIZE),
        price_ticks: rd8(SL_PRICE),
        maker_id: rd8(SL_MAKER_ID),
        taker_side: s[SL_TAKER_SIDE],
        taker_sub_index: s[SL_TAKER_SUB],
        maker_sub_index: s[SL_MAKER_SUB],
        taker_was_jit: s[SL_JIT],
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Formal proofs (Kani). Run: `cargo kani --features no-entrypoint --harness <name>`.
// The no-silent-overwrite property — the #1 correctness risk the orderbook survey
// flagged — is proved here from the ring's backpressure invariant.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(kani)]
mod proofs {
    /// THE key safety property: an outbox slot is never silently overwritten while
    /// its fill is still unsettled. Reusing physical slot `produced % fo_cap` at
    /// `produced` overwrites the fill written at `produced - fo_cap`; this proves
    /// that prior fill's index is `<= settled` (already consumed) for ALL states
    /// the matcher permits — given the ring backpressure (`depth <= ring_cap`,
    /// enforced by `ring_push`'s `Full`) and the matcher guard (`fo_cap >=
    /// ring_cap`). ∀ legal (fo_cap, ring_cap, produced, settled).
    #[kani::proof]
    fn outbox_no_silent_overwrite() {
        let fo_cap: u64 = kani::any();
        let ring_cap: u64 = kani::any();
        let produced: u64 = kani::any();
        let settled: u64 = kani::any();
        kani::assume(ring_cap >= 1 && ring_cap <= 1 << 20);
        kani::assume(fo_cap >= ring_cap); // matcher require: fo_cap >= ring_cap
        kani::assume(settled <= produced); // cursors monotonic (ring invariant)
        kani::assume(produced - settled <= ring_cap); // ring `Full` backpressure
        kani::assume(produced >= fo_cap); // a physical wrap has occurred
                                          // depth = produced - settled <= ring_cap <= fo_cap  ⇒  produced - fo_cap <= settled
        let prev_tenant_index = produced - fo_cap;
        assert!(prev_tenant_index <= settled);
    }

    /// The write index is always within the slot region for any `produced`.
    #[kani::proof]
    fn outbox_write_index_in_bounds() {
        let cap: u32 = kani::any();
        let produced: u64 = kani::any();
        kani::assume(cap >= 1);
        let idx = produced % cap as u64;
        assert!(idx < cap as u64);
    }

    /// `grow_fill_outbox` requires the outbox be DRAINED
    /// (`produced == settled`) before it changes `cap`. This proves that gate is
    /// exactly what makes the cap change safe. An occupied slot holds the fill at
    /// some index `idx` with `settled <= idx < produced`, physically at
    /// `idx % cap`; a grow to `new_cap` would relocate it to `idx % new_cap` — a
    /// DIFFERENT slot — so a reader would misread it. Here we prove the invariant
    /// that rules this out: **any pending index implies the outbox is NOT drained**
    /// (`produced != settled`). Contrapositive: the drained gate excludes every
    /// state with a live slot occupant, so no cap-change remap can ever move a
    /// still-unsettled fill. ∀ legal (produced, settled, idx).
    #[kani::proof]
    fn drained_grow_has_no_remappable_pending_slot() {
        let produced: u64 = kani::any();
        let settled: u64 = kani::any();
        let idx: u64 = kani::any();
        kani::assume(settled <= produced); // cursors monotonic (ring invariant)
        kani::assume(idx >= settled && idx < produced); // `idx` occupies a slot
                                                        // A live slot occupant ⇒ depth >= 1 ⇒ not drained. So the
                                                        // `produced == settled` grow gate provably admits no remappable entry.
        assert!(produced != settled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: u32 = 256;

    fn fresh(cap: u32) -> Vec<u8> {
        let mut d = vec![0u8; fill_outbox_account_len(cap as usize)];
        outbox_init(&mut d, &[7u8; 32], cap, 254).unwrap();
        d
    }

    #[test]
    fn account_len_matches_ring_geometry() {
        // header 64 + cap*96; at the ring cap 256 → 24,640 bytes ("Large" DLP class)
        assert_eq!(fill_outbox_account_len(256), 64 + 256 * 96);
        assert_eq!(fill_outbox_account_len(256), 24_640);
    }

    #[test]
    fn init_and_check_roundtrip() {
        let d = fresh(CAP);
        assert_eq!(outbox_check(&d, &[7u8; 32]).unwrap(), CAP);
        assert_eq!(outbox_produced(&d), 0);
    }

    #[test]
    fn check_rejects_wrong_market() {
        let d = fresh(CAP);
        assert_eq!(
            outbox_check(&d, &[9u8; 32]),
            Err(FillOutboxError::WrongMarket)
        );
    }

    #[test]
    fn check_rejects_bad_disc() {
        let mut d = fresh(CAP);
        d[0] = 0;
        assert_eq!(outbox_check(&d, &[7u8; 32]), Err(FillOutboxError::Corrupt));
    }

    #[test]
    fn check_rejects_truncated_or_mismatched_len() {
        let mut d = fresh(CAP);
        d.truncate(d.len() - 1); // length no longer header+cap*slot
        assert_eq!(outbox_check(&d, &[7u8; 32]), Err(FillOutboxError::Corrupt));
    }

    #[test]
    fn write_then_read_slot_roundtrip() {
        let mut d = fresh(CAP);
        let taker = [1u8; 32];
        let maker = [2u8; 32];
        outbox_write_slot(&mut d, CAP, 5, &taker, &maker, 42, 100_000, 777, 1, 3, 4, 1).unwrap();
        let s = outbox_read_slot(&d, CAP, 5).unwrap();
        assert_eq!(s.taker, taker);
        assert_eq!(s.maker, maker);
        assert_eq!(s.size_lots, 42);
        assert_eq!(s.price_ticks, 100_000);
        assert_eq!(s.maker_id, 777);
        assert_eq!(s.taker_side, 1);
        assert_eq!(s.taker_sub_index, 3);
        assert_eq!(s.maker_sub_index, 4);
        assert_eq!(s.taker_was_jit, 1);
    }

    #[test]
    fn lp_fill_marker_roundtrips() {
        let mut d = fresh(CAP);
        let zero = [0u8; 32]; // Pubkey::default() ⇒ LP virtual-quote fill
        outbox_write_slot(&mut d, CAP, 0, &[1u8; 32], &zero, 1, 1, 0, 0, 0, 0, 0).unwrap();
        assert_eq!(outbox_read_slot(&d, CAP, 0).unwrap().maker, zero);
    }

    #[test]
    fn write_out_of_range_fails_closed() {
        let mut d = fresh(CAP);
        assert_eq!(
            outbox_write_slot(&mut d, CAP, CAP as u64, &[0; 32], &[0; 32], 0, 0, 0, 0, 0, 0, 0),
            Err(FillOutboxError::OutOfRange)
        );
    }

    #[test]
    fn cursors_mirror_roundtrips() {
        let mut d = fresh(CAP);
        outbox_set_cursors(&mut d, 123, 100);
        assert_eq!(outbox_produced(&d), 123);
        let mut sb = [0u8; 8];
        sb.copy_from_slice(&d[OFF_SETTLED..OFF_SETTLED + 8]);
        assert_eq!(u64::from_le_bytes(sb), 100);
    }

    #[test]
    fn set_cap_then_check_after_grow() {
        // simulate a grow: realloc to a larger cap, zero the tail, stamp the cap.
        let new_cap = CAP + 64;
        let mut d = fresh(CAP);
        d.resize(fill_outbox_account_len(new_cap as usize), 0);
        outbox_set_cap(&mut d, new_cap);
        assert_eq!(outbox_check(&d, &[7u8; 32]).unwrap(), new_cap);
    }

    // The no-silent-overwrite invariant (bounded model). With `outbox.cap ==
    // ring.cap`, a slot is overwritten at `produced` only after its prior occupant
    // (`produced − cap`) is settled, BECAUSE the ring enforces
    // `produced − settled <= cap`. We model the ring's guarantee and assert the
    // overwritten slot's prior write index was already consumed.
    #[test]
    fn slot_reuse_only_after_consumption() {
        let cap: u64 = CAP as u64;
        // The ring permits a write at `produced` iff depth `produced - settled < cap`.
        // So whenever we write slot (produced % cap), the previous tenant of that
        // physical slot was written at `produced - cap`, and `produced - cap < settled`
        // ⇒ already settled. Check across a full wrap.
        for produced in cap..(cap * 3) {
            for settled in (produced.saturating_sub(cap))..=produced {
                // ring invariant precondition: depth <= cap
                if produced - settled > cap {
                    continue;
                }
                if produced >= cap {
                    let prev_writer = produced - cap;
                    // physical slot collision
                    assert_eq!(prev_writer % cap, produced % cap);
                    // the ring only let us reach this `produced` with depth <= cap,
                    // and a fresh write requires depth < cap (push fails at == cap),
                    // so settled > produced - cap = prev_writer ⇒ prev fill consumed.
                    if produced - settled < cap {
                        assert!(settled > prev_writer);
                    }
                }
            }
        }
    }
}
