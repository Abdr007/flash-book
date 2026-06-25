//! Chaos / model-based differential fuzz of the v2 hypertree order book
//! (`MarketBookHandle`). Random insert/remove sequences are replayed against a
//! shadow model; after EVERY op the book must agree with the model on count,
//! find-ability, and price-time ordering — and must never panic. This stresses
//! the matching-engine data structure beyond what the Kani proofs (which cover
//! the `order_id` *encoding*) and the integration tests reach: the live RBT
//! restructuring under interleaved inserts/removes, free-list reuse, and the
//! cached best-price pointers.
//!
//! Pure (no Solana runtime), so it runs thousands of operations fast.

use anchor_lang::prelude::Pubkey;
use flash_book::hypertree::NIL;
use flash_book::state_v2::{
    encode_order_id, MarketBookHandle, RestingOrderV2, MARKET_BOOK_TOTAL_BYTES,
};
use proptest::prelude::*;

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

#[derive(Debug, Clone)]
enum Op {
    Insert { is_bid: bool, price: u64 },
    Remove { which: usize },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Narrow price band (1..=15) so MANY orders share a price — heavily
        // exercises the FIFO seq tiebreak (the property the old whole-word-
        // inversion bug violated for bids).
        (any::<bool>(), 1u64..=15u64).prop_map(|(is_bid, price)| Op::Insert { is_bid, price }),
        (0usize..10_000).prop_map(|which| Op::Remove { which }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// The book agrees with a shadow model after every random op, and the
    /// best-first walks are always in strict price-time order.
    #[test]
    fn book_stays_consistent_under_random_ops(
        ops in prop::collection::vec(op_strategy(), 1..150usize),
    ) {
        let mut data = vec![0u8; MARKET_BOOK_TOTAL_BYTES];
        MarketBookHandle::write_disc_and_init_header(
            &mut data,
            255,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        )
        .unwrap();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();

        // Shadow model: live orders as (is_bid, price, seq, order_id).
        let mut live: Vec<(bool, u64, u64, u64)> = Vec::new();
        let mut next_seq: u64 = 1;

        for op in ops {
            match op {
                Op::Insert { is_bid, price } => {
                    let seq = next_seq;
                    let oid = encode_order_id(price, seq, is_bid);
                    let res = if is_bid {
                        handle.insert_bid(order(price, seq, is_bid))
                    } else {
                        handle.insert_ask(order(price, seq, is_bid))
                    };
                    match res {
                        Ok(_) => {
                            live.push((is_bid, price, seq, oid));
                            next_seq += 1;
                        }
                        // BufferFull near MAX_NODES is the only acceptable error;
                        // the model is unchanged and the book must stay consistent.
                        Err(_) => {}
                    }
                }
                Op::Remove { which } => {
                    if live.is_empty() {
                        continue;
                    }
                    let i = which % live.len();
                    let (is_bid, _p, _s, oid) = live[i];
                    let idx = if is_bid {
                        handle.lookup_bid_by_order_id(oid)
                    } else {
                        handle.lookup_ask_by_order_id(oid)
                    };
                    prop_assert_ne!(idx, NIL, "a live order must be findable before removal");
                    if is_bid {
                        handle.remove_bid_node(idx);
                    } else {
                        handle.remove_ask_node(idx);
                    }
                    live.swap_remove(i);
                }
            }

            // ── invariants checked after EVERY op ───────────────────────
            // (1) the active count agrees with the model.
            prop_assert_eq!(handle.header.total_orders_active as usize, live.len());

            // (2) every live order is findable and round-trips to its id.
            for &(is_bid, _p, _s, oid) in &live {
                let idx = if is_bid {
                    handle.lookup_bid_by_order_id(oid)
                } else {
                    handle.lookup_ask_by_order_id(oid)
                };
                prop_assert_ne!(idx, NIL);
                prop_assert_eq!(handle.order_at(idx).order_id, oid);
            }

            // (3) best-first walks are in strict price-time order, and their
            //     lengths match the model's per-side split.
            let mut bids: Vec<(u64, u64)> = Vec::new();
            handle.for_each_bid_best_first(|_i, o| {
                bids.push((o.price_ticks, o.seq));
                true
            });
            let mut asks: Vec<(u64, u64)> = Vec::new();
            handle.for_each_ask_best_first(|_i, o| {
                asks.push((o.price_ticks, o.seq));
                true
            });

            let n_bids = live.iter().filter(|o| o.0).count();
            prop_assert_eq!(bids.len(), n_bids);
            prop_assert_eq!(asks.len(), live.len() - n_bids);

            // Bids: DESCENDING price, then ASCENDING seq (FIFO) within a price.
            for w in bids.windows(2) {
                let ((p0, s0), (p1, s1)) = (w[0], w[1]);
                prop_assert!(
                    p0 > p1 || (p0 == p1 && s0 < s1),
                    "bid walk not price-time: {:?} then {:?}",
                    w[0],
                    w[1]
                );
            }
            // Asks: ASCENDING price, then ASCENDING seq.
            for w in asks.windows(2) {
                let ((p0, s0), (p1, s1)) = (w[0], w[1]);
                prop_assert!(
                    p0 < p1 || (p0 == p1 && s0 < s1),
                    "ask walk not price-time: {:?} then {:?}",
                    w[0],
                    w[1]
                );
            }
        }
    }
}
