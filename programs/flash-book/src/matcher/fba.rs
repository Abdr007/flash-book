//! Frequent Batch Auction — Walrasian uniform-price clearing.
//!
//! Operates entirely in integer lot space. No floating point.
//!
//! Algorithm:
//!   1. Build the set of candidate prices = union of all order limit prices.
//!   2. For each candidate p, compute D(p) = Σ buys with limit ≥ p,
//!      S(p) = Σ sells with limit ≤ p, V(p) = min(D(p), S(p)).
//!   3. Choose p* maximizing V(p*). Tie-break by proximity to prior mark,
//!      then midpoint of the indifference interval if it spans the mark.
//!   4. Match eligible orders by (priority, seq) FIFO.
//!
//! Compute budget: O(N²) where N = orders in batch. With MAX_ORDERS_PER_BATCH
//! = 256, that's ~65K candidate-price evaluations per batch — well under
//! the 1.4M CU budget.

use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use crate::constants::LOT_EPSILON;
use crate::errors::FlashBookError;
use anchor_lang::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    pub taker_id: u64,
    pub maker_id: u64,
    pub taker_trader: Pubkey,
    pub maker_trader: Pubkey,
    pub taker_side: Side,
    pub size: BaseLots,
    pub price: Ticks,
}

#[derive(Debug, Clone)]
pub struct ClearResult {
    pub clearing_price: Ticks,
    pub clearing_volume: BaseLots,
    pub fills: Vec<Fill>,
}

/// Clear a batch. Orders is the full input set (limits + virtuals + revealed
/// takers + liquidations). `prior_mark` is used for tie-breaking.
pub fn clear_batch(orders: &[Order], prior_mark: Ticks) -> Result<ClearResult> {
    if orders.is_empty() {
        return Ok(ClearResult {
            clearing_price: prior_mark,
            clearing_volume: BaseLots::ZERO,
            fills: vec![],
        });
    }

    // Partition.
    let buys: Vec<&Order> = orders.iter().filter(|o| o.side == Side::Long).collect();
    let sells: Vec<&Order> = orders.iter().filter(|o| o.side == Side::Short).collect();

    if buys.is_empty() || sells.is_empty() {
        return Ok(ClearResult {
            clearing_price: prior_mark,
            clearing_volume: BaseLots::ZERO,
            fills: vec![],
        });
    }

    // ── O(N log N) clearing-price search (wave 22 phase 6) ─────────────
    //
    // Replaces the original O(N²) per-price-loop with a single sort
    // pass + monotone two-pointer walk. Because demand D(p) is
    // non-increasing in p and supply S(p) is non-decreasing in p, we
    // can maintain running running cumulative sums while walking
    // candidate prices in ascending order — each buy/sell is touched
    // exactly once.
    //
    //   D(p) = sum of buys.size where buys.limit_price >= p
    //   S(p) = sum of sells.size where sells.limit_price <= p
    //
    // Sort buys ascending by limit_price; sort sells ascending by
    // limit_price; walk prices ascending. As p rises:
    //   • buys with limit < p become ineligible → subtract from D
    //   • sells with limit <= p become eligible → add to S
    //
    // This lifts the practical cap on MAX_BATCH_ORDERS_PER_SIDE_V2
    // (wave 22 phase 6 bumps 64 → 256) without blowing the BPF CU
    // budget.

    // Sort buys + sells by limit_price ascending (clone refs).
    let mut buys_sorted: Vec<&Order> = buys.clone();
    buys_sorted.sort_by_key(|o| o.limit_price.0);
    let mut sells_sorted: Vec<&Order> = sells.clone();
    sells_sorted.sort_by_key(|o| o.limit_price.0);

    // Total buy size = D(0); we subtract as p rises and buys drop out.
    let mut total_buy_size: u64 = 0;
    for o in &buys_sorted {
        total_buy_size = total_buy_size
            .checked_add(o.size.0)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    }

    // Sorted unique candidate prices.
    let mut prices: Vec<Ticks> = orders.iter().map(|o| o.limit_price).collect();
    prices.sort_by_key(|t| t.0);
    prices.dedup();

    let mut best_volume: u64 = 0;
    let mut best_prices: Vec<Ticks> = Vec::new();

    let mut buy_ptr: usize = 0; // first buy with limit_price >= current p
    let mut sell_ptr: usize = 0; // first sell with limit_price > current p
    let mut d_running: u64 = total_buy_size;
    let mut s_running: u64 = 0;

    for p in &prices {
        // Drop buys with limit_price < p from D.
        while buy_ptr < buys_sorted.len()
            && buys_sorted[buy_ptr].limit_price.0 < p.0
        {
            d_running = d_running
                .checked_sub(buys_sorted[buy_ptr].size.0)
                .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
            buy_ptr += 1;
        }
        // Add sells with limit_price <= p to S.
        while sell_ptr < sells_sorted.len()
            && sells_sorted[sell_ptr].limit_price.0 <= p.0
        {
            s_running = s_running
                .checked_add(sells_sorted[sell_ptr].size.0)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            sell_ptr += 1;
        }

        let v = d_running.min(s_running);
        match v.cmp(&best_volume) {
            std::cmp::Ordering::Greater => {
                best_volume = v;
                best_prices.clear();
                best_prices.push(*p);
            }
            std::cmp::Ordering::Equal if v > 0 => {
                best_prices.push(*p);
            }
            _ => {}
        }
    }

    if best_volume == 0 || best_prices.is_empty() {
        return Ok(ClearResult {
            clearing_price: prior_mark,
            clearing_volume: BaseLots::ZERO,
            fills: vec![],
        });
    }

    // Tie-break. Defense in depth: even though `best_prices` is guaranteed
    // non-empty here (we returned early above on empty), use safe fallbacks
    // instead of `unwrap` so a future refactor can't introduce a panic path.
    let clearing_price = if best_prices.len() == 1 {
        best_prices[0]
    } else {
        let min_p = best_prices
            .iter()
            .min_by_key(|t| t.0)
            .copied()
            .unwrap_or(prior_mark)
            .0;
        let max_p = best_prices
            .iter()
            .max_by_key(|t| t.0)
            .copied()
            .unwrap_or(prior_mark)
            .0;
        if prior_mark.0 >= min_p && prior_mark.0 <= max_p {
            prior_mark
        } else {
            // Closest to prior mark.
            let mut closest = best_prices[0];
            let mut best_dist = abs_diff(closest.0, prior_mark.0);
            for p in &best_prices[1..] {
                let d = abs_diff(p.0, prior_mark.0);
                if d < best_dist {
                    best_dist = d;
                    closest = *p;
                }
            }
            closest
        }
    };

    // Filter eligible and sort by FIFO key.
    let mut eligible_buys: Vec<Order> = buys
        .iter()
        .filter(|o| o.limit_price.0 >= clearing_price.0)
        .map(|o| **o)
        .collect();
    let mut eligible_sells: Vec<Order> = sells
        .iter()
        .filter(|o| o.limit_price.0 <= clearing_price.0)
        .map(|o| **o)
        .collect();
    eligible_buys.sort_by_key(|o| o.fifo_key());
    eligible_sells.sort_by_key(|o| o.fifo_key());

    let mut fills: Vec<Fill> = Vec::new();
    let mut buy_idx = 0usize;
    let mut sell_idx = 0usize;
    let mut buy_remaining = eligible_buys
        .first()
        .map(|o| o.size.0)
        .unwrap_or(0);
    let mut sell_remaining = eligible_sells
        .first()
        .map(|o| o.size.0)
        .unwrap_or(0);

    while buy_idx < eligible_buys.len() && sell_idx < eligible_sells.len() {
        let buy = &eligible_buys[buy_idx];
        let sell = &eligible_sells[sell_idx];

        // Self-trade prevention. Apply the STP mode of the NEWER order
        // (larger seq); it's the order being placed against the resting
        // book, so its trader's preference wins.
        if buy.trader == sell.trader {
            let newer_is_buy = buy.seq > sell.seq;
            let mode = if newer_is_buy { buy.stp_mode } else { sell.stp_mode };
            match mode {
                crate::matcher::order::StpMode::CancelNewest => {
                    if newer_is_buy {
                        buy_idx += 1;
                        buy_remaining =
                            eligible_buys.get(buy_idx).map(|o| o.size.0).unwrap_or(0);
                    } else {
                        sell_idx += 1;
                        sell_remaining =
                            eligible_sells.get(sell_idx).map(|o| o.size.0).unwrap_or(0);
                    }
                }
                crate::matcher::order::StpMode::CancelOldest => {
                    if newer_is_buy {
                        sell_idx += 1;
                        sell_remaining =
                            eligible_sells.get(sell_idx).map(|o| o.size.0).unwrap_or(0);
                    } else {
                        buy_idx += 1;
                        buy_remaining =
                            eligible_buys.get(buy_idx).map(|o| o.size.0).unwrap_or(0);
                    }
                }
                crate::matcher::order::StpMode::CancelBoth => {
                    buy_idx += 1;
                    sell_idx += 1;
                    buy_remaining =
                        eligible_buys.get(buy_idx).map(|o| o.size.0).unwrap_or(0);
                    sell_remaining =
                        eligible_sells.get(sell_idx).map(|o| o.size.0).unwrap_or(0);
                }
            }
            continue;
        }

        let fill_size = buy_remaining.min(sell_remaining);
        if fill_size == 0 {
            break;
        }

        // Determine taker side for fee assignment.
        let buy_is_taker = buy.order_type.is_taker();
        let sell_is_taker = sell.order_type.is_taker();
        let (taker, maker) = if buy_is_taker && !sell_is_taker {
            (buy, sell)
        } else if sell_is_taker && !buy_is_taker {
            (sell, buy)
        } else {
            // Both takers or both makers — pick by priority (taker rank wins).
            if buy.order_type.priority() <= sell.order_type.priority() {
                (buy, sell)
            } else {
                (sell, buy)
            }
        };

        fills.push(Fill {
            taker_id: taker.id,
            maker_id: maker.id,
            taker_trader: taker.trader,
            maker_trader: maker.trader,
            taker_side: taker.side,
            size: BaseLots(fill_size),
            price: clearing_price,
        });

        buy_remaining = buy_remaining.checked_sub(fill_size).ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        sell_remaining = sell_remaining.checked_sub(fill_size).ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        if buy_remaining < LOT_EPSILON {
            buy_idx += 1;
            buy_remaining = eligible_buys.get(buy_idx).map(|o| o.size.0).unwrap_or(0);
        }
        if sell_remaining < LOT_EPSILON {
            sell_idx += 1;
            sell_remaining = eligible_sells.get(sell_idx).map(|o| o.size.0).unwrap_or(0);
        }
    }

    Ok(ClearResult {
        clearing_price,
        clearing_volume: BaseLots(best_volume),
        fills,
    })
}

fn abs_diff(a: u64, b: u64) -> u64 {
    a.abs_diff(b)
}

/// Used by the engine to know which orders went unfilled.
pub fn unfilled_remainders(
    orders: &[Order],
    fills: &[Fill],
) -> Vec<(u64, BaseLots)> {
    let mut filled_by_id: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    for f in fills {
        *filled_by_id.entry(f.taker_id).or_default() += f.size.0;
        *filled_by_id.entry(f.maker_id).or_default() += f.size.0;
    }
    let mut out = Vec::new();
    for o in orders {
        let filled = filled_by_id.get(&o.id).copied().unwrap_or(0);
        if filled < o.size.0 {
            out.push((o.id, BaseLots(o.size.0 - filled)));
        }
    }
    out
}

// Doc-test marker for OrderType used implicitly above.
const _: OrderType = OrderType::Limit;
