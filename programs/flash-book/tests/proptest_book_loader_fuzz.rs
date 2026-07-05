//! Adversarial byte-fuzz of the committed-book LOADER — the totality guarantee
//! the ER seam depends on. After an undelegate, `process_undelegation` hands
//! `MarketBookHandle::validate_node_links` a byte buffer produced by a
//! (potentially malicious or buggy) sequencer, and every subsequent hot-path op
//! runs `MarketBookHandle::from_account_data` on it. Both MUST be total against
//! arbitrary bytes: never panic, never read out of bounds, never loop forever —
//! only cleanly accept a well-formed book or reject a malformed one with an
//! error. `proptest_book.rs` fuzzes the tree under VALID operations; this fuzzes
//! the loader under ARBITRARY and MUTATED-VALID bytes, which is the actual
//! attack surface.
//!
//! On-chain account data is 8-byte aligned, so the buffers here are backed by a
//! `Vec<u64>` to reproduce that faithfully (the header is read via a `bytemuck`
//! cast at offset 8, which requires 8-alignment).

use anchor_lang::prelude::Pubkey;
use flash_book::state_v2::{
    encode_order_id, MarketBookHandle, RestingOrderV2, MARKET_BOOK_DISC,
    MARKET_BOOK_MAX_TOTAL_BYTES, MARKET_BOOK_PREFIX_BYTES, MARKET_BOOK_TOTAL_BYTES,
    NODE_TOTAL_BYTES,
};
use proptest::prelude::*;

/// Run `f` with an 8-aligned `&mut [u8]` of exactly `len` bytes, pre-filled from
/// `seed` (truncated/zero-padded to `len`). Backed by a live `Vec<u64>` so the
/// slice base — and thus every field offset — is 8-aligned like a real account.
fn with_aligned<R>(len: usize, seed: &[u8], f: impl FnOnce(&mut [u8]) -> R) -> R {
    let words = len.div_ceil(8).max(1);
    let mut backing = vec![0u64; words];
    // SAFETY: `backing` owns `words * 8 >= len` bytes and stays alive for the
    // whole call; we only ever touch the first `len` of them.
    let bytes: &mut [u8] =
        unsafe { std::slice::from_raw_parts_mut(backing.as_mut_ptr() as *mut u8, len) };
    let n = seed.len().min(len);
    bytes[..n].copy_from_slice(&seed[..n]);
    f(bytes)
}

/// A representative spread of lengths: below the minimum, the boundaries, a
/// non-node-aligned dynamic region, valid grown sizes, and just past the cap.
fn choose_len(i: usize) -> usize {
    let valid_grown = MARKET_BOOK_TOTAL_BYTES + NODE_TOTAL_BYTES * 3;
    match i % 9 {
        0 => 0,
        1 => 8,
        2 => MARKET_BOOK_PREFIX_BYTES,
        3 => MARKET_BOOK_TOTAL_BYTES - 1,
        4 => MARKET_BOOK_TOTAL_BYTES,
        5 => MARKET_BOOK_TOTAL_BYTES + 1, // dynamic region not a whole node
        6 => valid_grown,
        7 => MARKET_BOOK_MAX_TOTAL_BYTES,
        _ => MARKET_BOOK_MAX_TOTAL_BYTES + NODE_TOTAL_BYTES, // over the cap
    }
}

fn order(price: u64, seq: u64, is_bid: bool) -> RestingOrderV2 {
    RestingOrderV2 {
        order_id: encode_order_id(price, seq, is_bid),
        seq,
        price_ticks: price,
        size_lots: 1,
        expires_at_slot: 0,
        trader: Pubkey::default(),
        last_valid_slot: 0,
        side: if is_bid { 0 } else { 1 },
        order_type: 0,
        flags: 0,
        sub_index: 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Arbitrary bytes at every length class — the loader must return
    /// (Ok or Err), never panic / read OOB / loop. Stamping the real
    /// discriminator on ~half the cases pushes past the disc gate so the deeper
    /// header-index and node-link validation is exercised on garbage.
    #[test]
    fn loader_is_total_on_arbitrary_bytes(
        seed in prop::collection::vec(any::<u8>(), 0..512),
        len_choice in 0usize..9,
        stamp_disc in any::<bool>(),
    ) {
        let len = choose_len(len_choice);
        with_aligned(len, &seed, |data| {
            if stamp_disc && len >= 8 {
                data[..8].copy_from_slice(&MARKET_BOOK_DISC);
            }
            // The whole point: these calls must not panic for ANY input.
            let _ = MarketBookHandle::from_account_data(data);
            let snapshot = data.to_vec();
            let _ = MarketBookHandle::validate_node_links(&snapshot);
        });
    }

    /// Build a genuinely valid book, then flip arbitrary bytes anywhere in it and
    /// re-run the loader. This reaches the header roots / best pointers / free
    /// list / node links with plausible-but-corrupt values — the states a
    /// tampered commit actually produces — and asserts totality on them too.
    #[test]
    fn loader_is_total_on_mutated_valid_book(
        inserts in prop::collection::vec((any::<bool>(), 1u64..64, 1u64..500), 0..40),
        mutations in prop::collection::vec((any::<usize>(), any::<u8>()), 0..48),
    ) {
        // A valid book (16-aligned Vec, like the other book tests).
        let mut data = vec![0u8; MARKET_BOOK_TOTAL_BYTES];
        MarketBookHandle::write_disc_and_init_header(
            &mut data,
            0,
            Pubkey::default(),
            Pubkey::default(),
            Pubkey::default(),
        )
        .unwrap();
        {
            let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
            for (i, (is_bid, price, seq)) in inserts.iter().enumerate() {
                let o = order(*price, seq.wrapping_add(i as u64), *is_bid);
                if *is_bid {
                    let _ = handle.insert_bid(o);
                } else {
                    let _ = handle.insert_ask(o);
                }
            }
        }
        // A well-formed book always validates.
        prop_assert!(MarketBookHandle::validate_node_links(&data).is_ok());

        // Corrupt arbitrary bytes and re-run — must never panic, only accept or
        // cleanly reject.
        let len = data.len();
        for (off, val) in mutations {
            data[off % len] = val;
        }
        let mut probe = data.clone();
        let _ = MarketBookHandle::from_account_data(&mut probe);
        let _ = MarketBookHandle::validate_node_links(&data);
    }
}
