//! Settlement-authenticity Fill Commitment Queue (H1 part B / issue #35).
//!
//! The on-chain matcher (`place_taker_order_v2`) already crosses takers against
//! real resting orders and mutates the hypertree book — so the *authentic* fills
//! exist on-chain the moment they are produced. Settlement (`apply_fill`),
//! however, trusts the sequencer's fill data at face value. This module binds the
//! two: the matcher PUSHES a commitment for every fill it produces; settlement
//! RECOMPUTES the commitment from its arguments and may only CONSUME a matching,
//! oldest-pending entry. A compromised sequencer therefore cannot fabricate a
//! fill — it cannot produce a commitment the honest matcher never wrote.
//!
//! Design split for verifiability:
//!   * This module owns the **canonical preimage** (`fill_preimage`) and the
//!     **ring state machine** (`ring_push` / `ring_settle`). Both are pure and
//!     Kani-checkable — no syscalls, no account types.
//!   * The handler owns the **hash**: it keccak-hashes `fill_preimage(..)` via the
//!     Solana syscall to obtain the 32-byte `FillCommit`. Keeping the hash out of
//!     here keeps the state-machine proofs tractable; collision-resistance of
//!     keccak is the (stated) cryptographic assumption, not a Kani obligation.
//!
//! Composes with the H1 part-A monotonic `fill_seq` replay guard: part A stops a
//! *replayed* settlement, this stops a *fabricated* one.

/// 32-byte commitment to a single fill — `keccak256(fill_preimage(..))`, computed
/// by the caller. Opaque to this module (compared for equality only).
pub type FillCommit = [u8; 32];

/// PDA seed for the per-market `FillCommitmentAccount`: `[FILL_COMMIT_SEED, market]`.
pub const FILL_COMMIT_SEED: &[u8] = b"fill_commit";

/// Default ring capacity (pending unsettled fills a market may hold before the
/// matcher applies backpressure). 64 covers a deep taker sweep; settlement drains
/// it. Sized into the account at init; the account is realloc-expandable later.
pub const FILL_RING_CAP: u32 = 64;

/// Canonical fill-commitment preimage length (see `fill_preimage`).
pub const FILL_PREIMAGE_LEN: usize = 136;

/// Domain-separation tag so a fill commitment can never collide with any other
/// keccak preimage the program hashes.
pub const FILL_COMMIT_DOMAIN: [u8; 8] = *b"FBfillC1";

/// Pure ring-state-machine errors. The handler maps these onto `FlashBookError`
/// (`FillRingFull`/`FillRingEmpty`/`FillNotCommitted`/`FillRingCorrupt`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillRingError {
    /// Ring is at capacity — settlement must drain before the matcher pushes more.
    Full,
    /// No pending committed fill to settle.
    Empty,
    /// Counters are inconsistent (`settled > produced`) — corruption / tamper.
    Corrupt,
    /// Recomputed commitment does not match the oldest pending entry — the fill
    /// was fabricated or presented out of order.
    NotCommitted,
}

/// Canonical, byte-stable serialization of a fill's full economic content, bound
/// to its production index. The matcher and settlement build this identically;
/// hashing it yields the `FillCommit`. Layout (little-endian integers):
///
/// | off | len | field             |
/// |-----|-----|-------------------|
/// |   0 |   8 | domain tag        |
/// |   8 |  32 | market            |
/// |  40 |  32 | taker             |
/// |  72 |  32 | maker             |
/// | 104 |   1 | taker_side        |
/// | 105 |   1 | taker_sub_index   |
/// | 106 |   1 | maker_sub_index   |
/// | 107 |   8 | size_lots         |
/// | 115 |   8 | price_ticks       |
/// | 123 |   8 | produced_index    |
/// | 131 |   5 | zero pad          |
#[allow(clippy::too_many_arguments)]
pub fn fill_preimage(
    market: &[u8; 32],
    taker: &[u8; 32],
    maker: &[u8; 32],
    taker_side: u8,
    size_lots: u64,
    price_ticks: u64,
    taker_sub_index: u8,
    maker_sub_index: u8,
    produced_index: u64,
) -> [u8; FILL_PREIMAGE_LEN] {
    let mut p = [0u8; FILL_PREIMAGE_LEN];
    p[0..8].copy_from_slice(&FILL_COMMIT_DOMAIN);
    p[8..40].copy_from_slice(market);
    p[40..72].copy_from_slice(taker);
    p[72..104].copy_from_slice(maker);
    p[104] = taker_side;
    p[105] = taker_sub_index;
    p[106] = maker_sub_index;
    p[107..115].copy_from_slice(&size_lots.to_le_bytes());
    p[115..123].copy_from_slice(&price_ticks.to_le_bytes());
    p[123..131].copy_from_slice(&produced_index.to_le_bytes());
    // bytes 131..136 stay zero
    p
}

/// Number of pending (produced-but-unsettled) commitments. `Corrupt` if the
/// settled cursor has somehow passed the produced cursor.
#[inline]
pub fn ring_depth(produced: u64, settled: u64) -> Result<u64, FillRingError> {
    produced.checked_sub(settled).ok_or(FillRingError::Corrupt)
}

/// Producer side (matcher): append `commit` for a fill just crossed on-chain.
/// FIFO; fails `Full` at capacity (backpressure — never overwrites a pending,
/// unsettled commitment). `produced` advances iff the push succeeds.
pub fn ring_push(
    produced: &mut u64,
    settled: u64,
    slots: &mut [FillCommit],
    commit: FillCommit,
) -> Result<(), FillRingError> {
    let cap = slots.len() as u64;
    if cap == 0 {
        return Err(FillRingError::Corrupt);
    }
    let depth = ring_depth(*produced, settled)?;
    if depth >= cap {
        return Err(FillRingError::Full);
    }
    let idx = (*produced % cap) as usize;
    slots[idx] = commit;
    *produced = produced.checked_add(1).ok_or(FillRingError::Corrupt)?;
    Ok(())
}

/// Consumer side (settlement): the caller passes `recomputed = keccak(preimage)`
/// built from the fill it is about to settle. It must equal the oldest pending
/// entry (authenticity + FIFO). On success the slot is zeroed (consume-and-clear)
/// and `settled` advances — so the same physical entry can never settle twice.
/// `recomputed` not matching ⇒ `NotCommitted`, and `settled` is left untouched.
pub fn ring_settle(
    produced: u64,
    settled: &mut u64,
    slots: &mut [FillCommit],
    recomputed: FillCommit,
) -> Result<(), FillRingError> {
    let cap = slots.len() as u64;
    if cap == 0 {
        return Err(FillRingError::Corrupt);
    }
    let depth = ring_depth(produced, *settled)?;
    if depth == 0 {
        return Err(FillRingError::Empty);
    }
    let idx = (*settled % cap) as usize;
    if slots[idx] != recomputed {
        return Err(FillRingError::NotCommitted);
    }
    slots[idx] = [0u8; 32]; // consume-and-clear
    *settled = settled.checked_add(1).ok_or(FillRingError::Corrupt)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Account-buffer layer: the on-chain FillCommitmentAccount is a raw PDA (like
// MarketBookAccount) parsed by these functions — disc + fixed header + a flat
// trailing region of `cap` 32-byte slots. Keeping the layout here makes it
// host-unit-testable; the handler only does PDA validation + keccak. The slot
// region is viewed as `&mut [FillCommit]` via bytemuck (len always a multiple of
// 32), so the proven `ring_push` / `ring_settle` operate on it directly.
// ─────────────────────────────────────────────────────────────────────────────

/// 8-byte discriminator marking a raw account as a FillCommitmentAccount.
pub const FILL_COMMIT_DISC: [u8; 8] = *b"FBfcq\x00\x01\x00";

/// Fixed header length (disc + counters + cap + bump + pad + market pubkey).
pub const FILL_COMMIT_HEADER_LEN: usize = 64;

// Byte offsets within the account data.
const OFF_PRODUCED: usize = 8; // u64 LE
const OFF_SETTLED: usize = 16; // u64 LE
const OFF_CAP: usize = 24; // u32 LE
const OFF_BUMP: usize = 28; // u8
const OFF_MARKET: usize = 32; // [u8; 32]

/// Total account size for a given ring capacity.
pub const fn fill_commit_account_len(cap: usize) -> usize {
    FILL_COMMIT_HEADER_LEN + cap * 32
}

#[inline]
fn rd_u64(data: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[off..off + 8]);
    u64::from_le_bytes(b)
}
#[inline]
fn wr_u64(data: &mut [u8], off: usize, v: u64) {
    data[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// One-time initialize a freshly-allocated account buffer: stamp disc, market,
/// cap, bump; zero the counters and slots. `data.len()` must equal
/// `fill_commit_account_len(cap)`.
pub fn buffer_init(
    data: &mut [u8],
    market: &[u8; 32],
    cap: u32,
    bump: u8,
) -> Result<(), FillRingError> {
    if cap == 0 || data.len() != fill_commit_account_len(cap as usize) {
        return Err(FillRingError::Corrupt);
    }
    for b in data.iter_mut() {
        *b = 0;
    }
    data[0..8].copy_from_slice(&FILL_COMMIT_DISC);
    data[OFF_CAP..OFF_CAP + 4].copy_from_slice(&cap.to_le_bytes());
    data[OFF_BUMP] = bump;
    data[OFF_MARKET..OFF_MARKET + 32].copy_from_slice(market);
    Ok(())
}

/// Validate the discriminator, market binding, and self-consistent length.
pub fn buffer_check(data: &[u8], expected_market: &[u8; 32]) -> Result<u32, FillRingError> {
    if data.len() < FILL_COMMIT_HEADER_LEN || data[0..8] != FILL_COMMIT_DISC {
        return Err(FillRingError::Corrupt);
    }
    let mut capb = [0u8; 4];
    capb.copy_from_slice(&data[OFF_CAP..OFF_CAP + 4]);
    let cap = u32::from_le_bytes(capb);
    if cap == 0
        || data.len() != fill_commit_account_len(cap as usize)
        || &data[OFF_MARKET..OFF_MARKET + 32] != expected_market
    {
        return Err(FillRingError::Corrupt);
    }
    Ok(cap)
}

/// Current produced cursor = the index the NEXT pushed fill will carry (the
/// matcher must bind this into the preimage before hashing).
pub fn buffer_next_index(data: &[u8]) -> u64 {
    rd_u64(data, OFF_PRODUCED)
}

/// Current settled cursor = the index the NEXT settled fill must carry.
pub fn buffer_settle_index(data: &[u8]) -> u64 {
    rd_u64(data, OFF_SETTLED)
}

fn slot_view(data: &mut [u8], cap: usize) -> &mut [FillCommit] {
    let region = &mut data[FILL_COMMIT_HEADER_LEN..FILL_COMMIT_HEADER_LEN + cap * 32];
    bytemuck::cast_slice_mut::<u8, FillCommit>(region)
}

/// Producer: push a fill commitment (matcher). Reads/advances the produced
/// cursor in the header and writes the slot via the proven `ring_push`.
pub fn buffer_push(data: &mut [u8], market: &[u8; 32], commit: FillCommit) -> Result<(), FillRingError> {
    let cap = buffer_check(data, market)?;
    let mut produced = rd_u64(data, OFF_PRODUCED);
    let settled = rd_u64(data, OFF_SETTLED);
    {
        let slots = slot_view(data, cap as usize);
        ring_push(&mut produced, settled, slots, commit)?;
    }
    wr_u64(data, OFF_PRODUCED, produced);
    Ok(())
}

/// Consumer: settle (consume-and-clear) the oldest pending commitment
/// (settlement). `recomputed` is the handler's keccak of the fill it is settling.
pub fn buffer_settle(
    data: &mut [u8],
    market: &[u8; 32],
    recomputed: FillCommit,
) -> Result<(), FillRingError> {
    let cap = buffer_check(data, market)?;
    let produced = rd_u64(data, OFF_PRODUCED);
    let mut settled = rd_u64(data, OFF_SETTLED);
    {
        let slots = slot_view(data, cap as usize);
        ring_settle(produced, &mut settled, slots, recomputed)?;
    }
    wr_u64(data, OFF_SETTLED, settled);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// FV: machine-checked invariants for the consume-and-clear ring (Kani). These
// prove the STATE MACHINE (INV-S1/S2): settlement can never outrun production,
// the ring is depth-bounded, a fabricated/out-of-order fill is rejected, and a
// settled fill cannot settle again. Cryptographic authenticity (a forged
// `recomputed` matching a real entry) reduces to keccak collision-resistance,
// which is assumed, not proven here. Runs in the CI Kani job.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(kani)]
mod fill_commitment_kani_proofs {
    use super::*;

    /// Inductive: from ANY valid ring state, neither operation lets `settled`
    /// exceed `produced` (settlement never outruns matching — no settling a fill
    /// that was never produced). `ring_settle` on an empty ring returns `Empty`
    /// and leaves `settled` unchanged.
    #[kani::proof]
    fn ring_never_over_settles() {
        const CAP: usize = 3;
        let mut produced: u64 = kani::any();
        let mut settled: u64 = kani::any();
        // Bound the counters far below u64::MAX so the *harness's own* arithmetic
        // (settled + CAP, before + 1) cannot overflow; 2^40 fills exceeds any real
        // market lifetime. The ring code itself uses checked_add regardless.
        kani::assume(settled < (1u64 << 40));
        kani::assume(produced < (1u64 << 40));
        // valid starting state: settled ≤ produced ≤ settled + CAP
        kani::assume(settled <= produced);
        kani::assume(produced <= settled + CAP as u64);
        let mut slots: [FillCommit; CAP] = kani::any();
        let c: FillCommit = kani::any();

        if kani::any() {
            let before = settled;
            let _ = ring_push(&mut produced, settled, &mut slots, c);
            // push never touches `settled`
            assert!(settled == before);
            assert!(settled <= produced);
        } else {
            let before = settled;
            let r = ring_settle(produced, &mut settled, &mut slots, c);
            assert!(settled <= produced);
            if r.is_err() {
                assert!(settled == before); // a rejected settle advances nothing
            } else {
                assert!(settled == before + 1);
            }
        }
    }

    /// Inductive: the pending depth never exceeds capacity — `ring_push` at
    /// capacity returns `Full` and does NOT advance `produced` (no overwrite of a
    /// live, unsettled commitment, no wrap-aliasing).
    #[kani::proof]
    fn ring_depth_bounded() {
        const CAP: usize = 3;
        let mut produced: u64 = kani::any();
        let settled: u64 = kani::any();
        kani::assume(settled < (1u64 << 40));
        kani::assume(produced < (1u64 << 40));
        kani::assume(settled <= produced);
        kani::assume(produced <= settled + CAP as u64); // depth ≤ CAP precondition
        let mut slots: [FillCommit; CAP] = kani::any();
        let c: FillCommit = kani::any();

        let before = produced;
        let r = ring_push(&mut produced, settled, &mut slots, c);
        // depth stays within [0, CAP]
        assert!(produced >= settled);
        assert!(produced <= settled + CAP as u64);
        if r.is_err() {
            assert!(produced == before); // Full ⇒ no advance
        } else {
            assert!(produced == before + 1);
        }
    }

    /// A fill whose recomputed commitment does NOT equal the oldest pending entry
    /// is REJECTED, and `settled` is untouched. This is the anti-fabrication core:
    /// a sequencer cannot settle anything the matcher did not commit.
    #[kani::proof]
    fn settle_rejects_uncommitted() {
        const CAP: usize = 3;
        let produced: u64 = kani::any();
        let mut settled: u64 = kani::any();
        kani::assume(settled < (1u64 << 40));
        kani::assume(produced < (1u64 << 40));
        kani::assume(settled < produced); // depth ≥ 1 (something pending)
        kani::assume(produced <= settled + CAP as u64);
        let mut slots: [FillCommit; CAP] = kani::any();
        let recomputed: FillCommit = kani::any();
        let idx = (settled % CAP as u64) as usize;
        kani::assume(slots[idx] != recomputed); // does not match the tail

        let before = settled;
        let r = ring_settle(produced, &mut settled, &mut slots, recomputed);
        assert!(r == Err(FillRingError::NotCommitted));
        assert!(settled == before);
    }

    /// Consume-and-clear: a successful settle advances `settled` by exactly one
    /// AND zeroes the consumed physical slot — so that entry can never settle
    /// again (no double-spend of one matcher fill).
    #[kani::proof]
    fn no_double_settle() {
        const CAP: usize = 3;
        let produced: u64 = kani::any();
        let mut settled: u64 = kani::any();
        kani::assume(settled < (1u64 << 40));
        kani::assume(produced < (1u64 << 40));
        kani::assume(settled < produced);
        kani::assume(produced <= settled + CAP as u64);
        let mut slots: [FillCommit; CAP] = kani::any();
        let idx = (settled % CAP as u64) as usize;
        let tail = slots[idx];

        let before = settled;
        let r = ring_settle(produced, &mut settled, &mut slots, tail);
        assert!(r.is_ok());
        assert!(settled == before + 1);
        assert!(slots[idx] == [0u8; 32]); // consumed slot cleared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(n: u8) -> FillCommit {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    #[test]
    fn fifo_push_then_settle_in_order() {
        let mut produced = 0u64;
        let mut settled = 0u64;
        let mut slots = [[0u8; 32]; 4];
        // matcher produces three fills
        ring_push(&mut produced, settled, &mut slots, c(1)).unwrap();
        ring_push(&mut produced, settled, &mut slots, c(2)).unwrap();
        ring_push(&mut produced, settled, &mut slots, c(3)).unwrap();
        assert_eq!(ring_depth(produced, settled).unwrap(), 3);
        // settlement drains them FIFO
        ring_settle(produced, &mut settled, &mut slots, c(1)).unwrap();
        ring_settle(produced, &mut settled, &mut slots, c(2)).unwrap();
        ring_settle(produced, &mut settled, &mut slots, c(3)).unwrap();
        assert_eq!(ring_depth(produced, settled).unwrap(), 0);
    }

    #[test]
    fn fabricated_fill_rejected() {
        let mut produced = 0u64;
        let mut settled = 0u64;
        let mut slots = [[0u8; 32]; 4];
        ring_push(&mut produced, settled, &mut slots, c(1)).unwrap();
        // sequencer tries to settle a fill the matcher never produced
        assert_eq!(
            ring_settle(produced, &mut settled, &mut slots, c(99)),
            Err(FillRingError::NotCommitted)
        );
        // the real one still settles
        ring_settle(produced, &mut settled, &mut slots, c(1)).unwrap();
    }

    #[test]
    fn out_of_order_rejected() {
        let mut produced = 0u64;
        let mut settled = 0u64;
        let mut slots = [[0u8; 32]; 4];
        ring_push(&mut produced, settled, &mut slots, c(1)).unwrap();
        ring_push(&mut produced, settled, &mut slots, c(2)).unwrap();
        // try to settle #2 before #1 — FIFO rejects
        assert_eq!(
            ring_settle(produced, &mut settled, &mut slots, c(2)),
            Err(FillRingError::NotCommitted)
        );
    }

    #[test]
    fn backpressure_when_full() {
        let mut produced = 0u64;
        let settled = 0u64;
        let mut slots = [[0u8; 32]; 2];
        ring_push(&mut produced, settled, &mut slots, c(1)).unwrap();
        ring_push(&mut produced, settled, &mut slots, c(2)).unwrap();
        assert_eq!(
            ring_push(&mut produced, settled, &mut slots, c(3)),
            Err(FillRingError::Full)
        );
    }

    #[test]
    fn double_settle_impossible() {
        let mut produced = 0u64;
        let mut settled = 0u64;
        let mut slots = [[0u8; 32]; 4];
        ring_push(&mut produced, settled, &mut slots, c(1)).unwrap();
        ring_settle(produced, &mut settled, &mut slots, c(1)).unwrap();
        // the consumed slot is cleared and settled advanced — re-presenting c(1)
        // finds nothing pending
        assert_eq!(
            ring_settle(produced, &mut settled, &mut slots, c(1)),
            Err(FillRingError::Empty)
        );
    }

    // ── account-buffer layer ─────────────────────────────────────────────
    const TEST_CAP: u32 = 8;
    fn fresh_buffer(market: &[u8; 32]) -> Vec<u8> {
        let mut data = vec![0u8; fill_commit_account_len(TEST_CAP as usize)];
        buffer_init(&mut data, market, TEST_CAP, 254).unwrap();
        data
    }

    #[test]
    fn buffer_init_and_check() {
        let market = [7u8; 32];
        let data = fresh_buffer(&market);
        assert_eq!(&data[0..8], &FILL_COMMIT_DISC);
        assert_eq!(buffer_check(&data, &market).unwrap(), TEST_CAP);
        assert_eq!(buffer_next_index(&data), 0);
        assert_eq!(buffer_settle_index(&data), 0);
        // wrong market is rejected
        assert_eq!(buffer_check(&data, &[9u8; 32]), Err(FillRingError::Corrupt));
    }

    #[test]
    fn buffer_produce_then_settle_roundtrip() {
        let market = [3u8; 32];
        let mut data = fresh_buffer(&market);
        // produce three; cursor advances
        buffer_push(&mut data, &market, c(11)).unwrap();
        buffer_push(&mut data, &market, c(12)).unwrap();
        buffer_push(&mut data, &market, c(13)).unwrap();
        assert_eq!(buffer_next_index(&data), 3);
        assert_eq!(buffer_settle_index(&data), 0);
        // settle FIFO
        buffer_settle(&mut data, &market, c(11)).unwrap();
        buffer_settle(&mut data, &market, c(12)).unwrap();
        assert_eq!(buffer_settle_index(&data), 2);
        // a fabricated fill is rejected, cursor unmoved
        assert_eq!(
            buffer_settle(&mut data, &market, c(99)),
            Err(FillRingError::NotCommitted)
        );
        assert_eq!(buffer_settle_index(&data), 2);
        buffer_settle(&mut data, &market, c(13)).unwrap();
        assert_eq!(buffer_settle_index(&data), 3);
        // fully drained
        assert_eq!(buffer_settle(&mut data, &market, c(13)), Err(FillRingError::Empty));
    }

    #[test]
    fn buffer_backpressure_at_cap() {
        let market = [5u8; 32];
        let mut data = fresh_buffer(&market);
        for i in 0..TEST_CAP as u8 {
            buffer_push(&mut data, &market, c(i + 1)).unwrap();
        }
        assert_eq!(buffer_next_index(&data), TEST_CAP as u64);
        assert_eq!(buffer_push(&mut data, &market, c(200)), Err(FillRingError::Full));
    }

    #[test]
    fn buffer_wraps_around() {
        let market = [1u8; 32];
        let mut data = fresh_buffer(&market);
        // produce + settle 20 fills through an 8-slot ring (forces wrap)
        for i in 0..20u8 {
            buffer_push(&mut data, &market, c(i + 1)).unwrap();
            buffer_settle(&mut data, &market, c(i + 1)).unwrap();
        }
        assert_eq!(buffer_next_index(&data), 20);
        assert_eq!(buffer_settle_index(&data), 20);
    }
}
