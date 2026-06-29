//! Settlement-authenticity Fill Commitment Queue — pin port of the Anchor
//! `matcher::fill_commitment`. The matcher PUSHES a keccak commitment for every
//! fill it produces; settlement RECOMPUTES it and may only CONSUME a matching,
//! oldest-pending entry — so a compromised sequencer cannot fabricate a fill.
//!
//! Pure + host-testable: this module owns the canonical preimage, the ring state
//! machine, and the account-buffer byte layout. The keccak hash lives in the
//! handler (the `FillCommit` is opaque here, compared for equality only).

/// 32-byte commitment to a single fill — `keccak256(fill_preimage(..))`, computed
/// by the caller. Opaque here (compared for equality only).
pub type FillCommit = [u8; 32];

/// PDA seed for the per-market account: `[FILL_COMMIT_SEED, market]`.
pub const FILL_COMMIT_SEED: &[u8] = b"fill_commit";
/// Default ring capacity (pending unsettled fills before backpressure).
pub const FILL_RING_CAP: u32 = 64;
/// Canonical fill-commitment preimage length.
pub const FILL_PREIMAGE_LEN: usize = 136;
/// Domain-separation tag for the keccak preimage.
pub const FILL_COMMIT_DOMAIN: [u8; 8] = *b"FBfillC1";
/// 8-byte discriminator marking a raw account as a FillCommitmentAccount.
pub const FILL_COMMIT_DISC: [u8; 8] = *b"FBfcq\x00\x01\x00";
/// Fixed header length (disc + counters + cap + bump + pad + market pubkey).
pub const FILL_COMMIT_HEADER_LEN: usize = 64;

const OFF_PRODUCED: usize = 8;
const OFF_SETTLED: usize = 16;
const OFF_CAP: usize = 24;
const OFF_BUMP: usize = 28;
const OFF_MARKET: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillRingError {
    Full,
    Empty,
    Corrupt,
    NotCommitted,
}

/// Total account size for a given ring capacity.
pub const fn fill_commit_account_len(cap: usize) -> usize {
    FILL_COMMIT_HEADER_LEN + cap * 32
}

/// Canonical, byte-stable serialization of a fill's economic content bound to its
/// production index — hashing it yields the `FillCommit`. Faithful to anchor.
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
    p
}

/// Pending (produced-but-unsettled) commitment count; `Corrupt` if settled passed produced.
#[inline]
pub fn ring_depth(produced: u64, settled: u64) -> Result<u64, FillRingError> {
    produced.checked_sub(settled).ok_or(FillRingError::Corrupt)
}

/// Producer (matcher): append `commit`; FIFO, `Full` at capacity (no overwrite).
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

/// Consumer (settlement): `recomputed` must equal the oldest pending entry
/// (authenticity + FIFO); consume-and-clear, advance `settled`.
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
    slots[idx] = [0u8; 32];
    *settled = settled.checked_add(1).ok_or(FillRingError::Corrupt)?;
    Ok(())
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

/// SAFETY-equivalent of anchor's bytemuck view: the slot region is `cap * 32`
/// bytes and `FillCommit = [u8; 32]` has alignment 1, so this cast is sound.
#[inline]
fn slot_view(data: &mut [u8], cap: usize) -> &mut [FillCommit] {
    let region = &mut data[FILL_COMMIT_HEADER_LEN..FILL_COMMIT_HEADER_LEN + cap * 32];
    // align_to_mut on a 1-aligned target never produces a non-empty prefix.
    let (_pre, slots, _post) = unsafe { region.align_to_mut::<FillCommit>() };
    slots
}

/// One-time init of a freshly-allocated buffer: stamp disc/market/cap/bump, zero
/// counters + slots. `data.len()` must equal `fill_commit_account_len(cap)`.
pub fn buffer_init(data: &mut [u8], market: &[u8; 32], cap: u32, bump: u8) -> Result<(), FillRingError> {
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

/// Validate disc + market binding + self-consistent length; returns the capacity.
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

pub fn buffer_next_index(data: &[u8]) -> u64 {
    rd_u64(data, OFF_PRODUCED)
}
pub fn buffer_settle_index(data: &[u8]) -> u64 {
    rd_u64(data, OFF_SETTLED)
}

/// Producer: push a commitment (matcher) — advances the produced cursor.
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

/// Consumer: settle (consume-and-clear) the oldest pending commitment.
pub fn buffer_settle(data: &mut [u8], market: &[u8; 32], recomputed: FillCommit) -> Result<(), FillRingError> {
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

/// Advance the settlement nonce (H1 part A): `Ok(fill_seq)` iff `fill_seq > current`.
#[inline]
pub fn advance_settlement_seq(current: u64, fill_seq: u64) -> Result<u64, ()> {
    if fill_seq > current {
        Ok(fill_seq)
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: u8) -> FillCommit {
        [n; 32]
    }

    #[test]
    fn preimage_layout_is_byte_exact() {
        let p = fill_preimage(&[1; 32], &[2; 32], &[3; 32], 1, 7, 9, 4, 5, 11);
        assert_eq!(&p[0..8], b"FBfillC1");
        assert_eq!(&p[8..40], &[1u8; 32]);
        assert_eq!(p[104], 1); // side
        assert_eq!(&p[107..115], &7u64.to_le_bytes());
        assert_eq!(&p[123..131], &11u64.to_le_bytes());
        assert_eq!(&p[131..136], &[0u8; 5]); // pad
    }

    #[test]
    fn ring_fifo_push_settle() {
        let mut slots = [commit(0); 4];
        let (mut produced, mut settled) = (0u64, 0u64);
        ring_push(&mut produced, settled, &mut slots, commit(1)).unwrap();
        ring_push(&mut produced, settled, &mut slots, commit(2)).unwrap();
        assert_eq!(ring_depth(produced, settled), Ok(2));
        // settle FIFO: oldest (commit 1) first.
        ring_settle(produced, &mut settled, &mut slots, commit(1)).unwrap();
        // wrong recompute → NotCommitted, settled untouched.
        assert_eq!(ring_settle(produced, &mut settled, &mut slots, commit(9)), Err(FillRingError::NotCommitted));
        ring_settle(produced, &mut settled, &mut slots, commit(2)).unwrap();
        assert_eq!(ring_settle(produced, &mut settled, &mut slots, commit(2)), Err(FillRingError::Empty));
    }

    #[test]
    fn ring_full_backpressure() {
        let mut slots = [commit(0); 2];
        let (mut produced, settled) = (0u64, 0u64);
        ring_push(&mut produced, settled, &mut slots, commit(1)).unwrap();
        ring_push(&mut produced, settled, &mut slots, commit(2)).unwrap();
        assert_eq!(ring_push(&mut produced, settled, &mut slots, commit(3)), Err(FillRingError::Full));
    }

    #[test]
    fn buffer_init_check_roundtrip() {
        let cap = 4u32;
        let mut data = vec![0u8; fill_commit_account_len(cap as usize)];
        let market = [0x55u8; 32];
        buffer_init(&mut data, &market, cap, 7).unwrap();
        assert_eq!(buffer_check(&data, &market), Ok(cap));
        assert_eq!(buffer_check(&data, &[0x66; 32]), Err(FillRingError::Corrupt)); // wrong market
        assert_eq!(data[OFF_BUMP], 7);
        assert_eq!(buffer_next_index(&data), 0);
    }

    #[test]
    fn buffer_push_settle_via_bytes() {
        let cap = 4u32;
        let mut data = vec![0u8; fill_commit_account_len(cap as usize)];
        let market = [0x55u8; 32];
        buffer_init(&mut data, &market, cap, 1).unwrap();
        buffer_push(&mut data, &market, commit(1)).unwrap();
        buffer_push(&mut data, &market, commit(2)).unwrap();
        assert_eq!(buffer_next_index(&data), 2);
        assert_eq!(buffer_settle_index(&data), 0);
        buffer_settle(&mut data, &market, commit(1)).unwrap();
        assert_eq!(buffer_settle_index(&data), 1);
        // fabricated fill (never pushed) → NotCommitted.
        assert_eq!(buffer_settle(&mut data, &market, commit(7)), Err(FillRingError::NotCommitted));
    }

    #[test]
    fn settlement_seq_monotonic() {
        assert_eq!(advance_settlement_seq(5, 6), Ok(6));
        assert_eq!(advance_settlement_seq(5, 5), Err(())); // replay
        assert_eq!(advance_settlement_seq(5, 4), Err(())); // out of order
    }
}
