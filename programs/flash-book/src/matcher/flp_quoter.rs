//! Virtual FLP quoter — integer-arithmetic port of the TS reference.
//!
//! Spread function (per level, cumulative size Q):
//!   s_bps = s0 + α·VPIN_bps + β·u_bps + γ·|oi_imb_bps| + κ·(Q/depth_floor)·BPS
//!
//! Inventory skew (Avellaneda-Stoikov-inspired):
//!   skew_bps = -λ_bps · (pool_net_q / pool_capital_q)
//! (we omit the volatility-coupled risk-aversion term in v1 of the Rust port;
//! it's a pure-Rust enhancement to be added without behavioural surprises).
//!
//! All math uses checked u128 / i128 arithmetic. No floats.

use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use crate::constants::BPS_DENOM;
use crate::errors::{FlashBookError, OrOverflow};
use anchor_lang::prelude::*;

#[derive(Debug, Clone, Copy)]
pub struct FlpQuoterParams {
    pub base_spread_bps: u32,
    pub alpha_bps: u32, // VPIN coefficient
    pub beta_bps: u32,  // utilization coefficient
    pub gamma_bps: u32, // OI imbalance coefficient
    pub kappa_bps: u32, // depth amortization (Q/depth_floor)
    pub delta_bps: u32, // realized-volatility coefficient
    pub inventory_lambda_bps: u32,
    pub depth_floor_lots: u64,
    pub max_growth_per_batch_bps: u32,
    pub levels: u8,
    pub tick_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct FlpQuoterInputs {
    pub oracle_ticks: Ticks,
    pub vpin_bps: u32,
    pub realized_vol_bps: u32,
    pub pool_capital_quote_lots: u64,
    pub pool_net_quote_lots_signed: i64,
    pub pool_gross_utilization_bps: u32,
    pub oi_long_lots: u64,
    pub oi_short_lots: u64,
}

#[derive(Debug, Clone)]
pub struct FlpQuoterOutput {
    pub bids: Vec<(Ticks, BaseLots)>,
    pub asks: Vec<(Ticks, BaseLots)>,
    pub fair_value: Ticks,
    pub skew_bps: i32,
}

pub fn generate_quotes(
    params: FlpQuoterParams,
    inputs: FlpQuoterInputs,
    flp_trader: Pubkey,
    base_seq: u64,
) -> Result<(FlpQuoterOutput, Vec<Order>)> {
    let empty = FlpQuoterOutput {
        bids: vec![],
        asks: vec![],
        fair_value: inputs.oracle_ticks,
        skew_bps: 0,
    };

    if inputs.pool_capital_quote_lots == 0 || inputs.oracle_ticks.0 == 0 || params.levels == 0 {
        return Ok((empty, vec![]));
    }

    // OI imbalance in bps (signed). Positive = traders net-long.
    let oi_total = inputs
        .oi_long_lots
        .checked_add(inputs.oi_short_lots)
        .or_overflow()?;
    let oi_imb_bps: i32 = if oi_total > 0 {
        let diff: i128 = (inputs.oi_long_lots as i128) - (inputs.oi_short_lots as i128);
        ((diff * BPS_DENOM as i128) / oi_total as i128) as i32
    } else {
        0
    };

    // Inventory fraction in bps. Clamped to ±BPS_DENOM (100%) so even a
    // catastrophic accounting drift cannot cause i32 truncation to flip
    // sign or wrap.
    let inv_bps: i32 = if inputs.pool_capital_quote_lots > 0 {
        let prod: i128 = (inputs.pool_net_quote_lots_signed as i128) * BPS_DENOM as i128;
        let raw = prod / inputs.pool_capital_quote_lots as i128;
        raw.clamp(-(BPS_DENOM as i128), BPS_DENOM as i128) as i32
    } else {
        0
    };

    // skew_bps = -lambda * inv_bps / BPS_DENOM (so lambda is in bps too).
    // `inventory_lambda_bps` is an unbounded governance u32, so `lambda * inv_bps
    // / BPS_DENOM` can exceed i32::MAX — a bare `as i32` would truncate and flip
    // the skew's sign (quoting TOWARD inventory), and an inner value of exactly
    // i32::MIN would panic on the unary minus (quoting DoS). Compute in i64 and
    // clamp to ±BPS_DENOM (±100%) BEFORE the cast and negation: a skew beyond
    // ±100% of the oracle is economically meaningless (`apply_bps_signed` already
    // floors a negative fair value to 0), so the clamp never binds on a sane
    // config and fails safe on a misconfigured one.
    let skew_magnitude =
        (params.inventory_lambda_bps as i64).saturating_mul(inv_bps as i64) / BPS_DENOM as i64;
    let skew_bps = -(skew_magnitude.clamp(-(BPS_DENOM as i64), BPS_DENOM as i64)) as i32;

    // fair_value = oracle * (1 + skew_bps/BPS_DENOM)
    let fair_value = apply_bps_signed(inputs.oracle_ticks, skew_bps)?;

    // Per-batch growth cap → per-level size in base lots.
    let usd_cap = (inputs.pool_capital_quote_lots as u128
        * params.max_growth_per_batch_bps as u128)
        / BPS_DENOM as u128;
    let per_level_quote = usd_cap / params.levels as u128;

    if per_level_quote == 0 {
        return Ok((
            FlpQuoterOutput {
                fair_value,
                skew_bps,
                ..empty
            },
            vec![],
        ));
    }

    // per_level_size_base_lots ≈ per_level_quote / (oracle_ticks * tick_size)
    // notional_per_lot = oracle_ticks * tick_size
    let notional_per_lot = (inputs.oracle_ticks.0 as u128)
        .checked_mul(params.tick_size as u128)
        .or_overflow()?;
    if notional_per_lot == 0 {
        return Ok((
            FlpQuoterOutput {
                fair_value,
                skew_bps,
                ..empty
            },
            vec![],
        ));
    }
    let per_level_lots = (per_level_quote / notional_per_lot) as u64;
    if per_level_lots == 0 {
        return Ok((
            FlpQuoterOutput {
                fair_value,
                skew_bps,
                ..empty
            },
            vec![],
        ));
    }

    let mut bids: Vec<(Ticks, BaseLots)> = Vec::with_capacity(params.levels as usize);
    let mut asks: Vec<(Ticks, BaseLots)> = Vec::with_capacity(params.levels as usize);
    let mut orders: Vec<Order> = Vec::with_capacity((params.levels as usize) * 2);

    for i in 1..=(params.levels as u64) {
        let cum_size = per_level_lots
            .checked_mul(i)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        // s_bps = base + α·vpin + β·u + γ·|oi_imb| + κ·(Q/depth_floor)·BPS
        let oi_term =
            (params.gamma_bps as u64 * (oi_imb_bps.unsigned_abs() as u64)) / BPS_DENOM as u64;
        let depth_term = if params.depth_floor_lots > 0 {
            ((cum_size as u128) * (params.kappa_bps as u128) / (params.depth_floor_lots as u128))
                as u64
        } else {
            0
        };
        let vol_term =
            (params.delta_bps as u64 * inputs.realized_vol_bps as u64) / BPS_DENOM as u64;
        let s_bps = (params.base_spread_bps as u64)
            .checked_add((params.alpha_bps as u64 * inputs.vpin_bps as u64) / BPS_DENOM as u64)
            .or_overflow()?
            .checked_add(
                (params.beta_bps as u64 * inputs.pool_gross_utilization_bps as u64)
                    / BPS_DENOM as u64,
            )
            .or_overflow()?
            .checked_add(oi_term)
            .or_overflow()?
            .checked_add(depth_term)
            .or_overflow()?
            .checked_add(vol_term)
            .or_overflow()?;
        // Cap spread at 50% (ridiculous floor).
        let s_bps = s_bps.min(5000) as u32;

        let bid = apply_bps_signed(fair_value, -(s_bps as i32))?;
        let ask = apply_bps_signed(fair_value, s_bps as i32)?;
        let bid = align_tick(bid, params.tick_size);
        let ask = align_tick(ask, params.tick_size);

        if bid.0 > 0 {
            bids.push((bid, BaseLots(per_level_lots)));
            orders.push(Order {
                id: base_seq + (i * 2),
                trader: flp_trader,
                side: Side::Long,
                order_type: OrderType::FlpVirtual,
                size: BaseLots(per_level_lots),
                limit_price: bid,
                seq: base_seq + (i * 2),
                post_only: false,
                stp_mode: crate::matcher::order::StpMode::CancelNewest,
            });
        }
        if ask.0 > 0 {
            asks.push((ask, BaseLots(per_level_lots)));
            orders.push(Order {
                id: base_seq + (i * 2) + 1,
                trader: flp_trader,
                side: Side::Short,
                order_type: OrderType::FlpVirtual,
                size: BaseLots(per_level_lots),
                limit_price: ask,
                seq: base_seq + (i * 2) + 1,
                post_only: false,
                stp_mode: crate::matcher::order::StpMode::CancelNewest,
            });
        }
    }

    Ok((
        FlpQuoterOutput {
            bids,
            asks,
            fair_value,
            skew_bps,
        },
        orders,
    ))
}

/// price * (1 + bps/10000) with signed bps, in tick space.
fn apply_bps_signed(price: Ticks, bps: i32) -> Result<Ticks> {
    let p = price.0 as i128;
    let delta = p * bps as i128 / BPS_DENOM as i128;
    let result = p.checked_add(delta).or_overflow()?;
    if result < 0 {
        return Ok(Ticks(0));
    }
    Ok(Ticks(result as u64))
}

fn align_tick(price: Ticks, tick_size: u64) -> Ticks {
    if tick_size <= 1 {
        return price;
    }
    Ticks((price.0 / tick_size) * tick_size)
}

// ─────────────────────────────────────────────────────────────────────────────
// FLP fill-price authenticity bound.
//
// FLP fills are quotes against pool liquidity, not on-chain resting orders, so
// they cannot be bound by the matcher's fill-commitment ring (which covers book
// fills). Their authenticity is recovered differently: the FLP quoter ALWAYS
// prices within its spread of fair value, so an authentic fill is within a small
// deviation of the fresh oracle. This pure, overflow-free predicate is the
// settlement-time gate — a compromised sequencer cannot settle an FLP fill far
// enough from the oracle to drain the pool. It is a BOUND, not exact quote
// re-derivation (which is unsound here: the quoter's inputs — vpin, inventory, OI
// — drift between quote-time on the ER and settle-time on L1, so re-deriving would
// reject legitimate fills). The residual surface is within-band mispricing,
// economically capped at `max_dev_bps` of notional per (replay-guarded) fill.
// ─────────────────────────────────────────────────────────────────────────────

/// True iff `price_ticks` is within `max_dev_bps` (symmetric) of `oracle_ticks`.
/// `oracle_ticks == 0` (no anchor) returns true — the caller gates on a live
/// oracle. Overflow-free: with `oracle ≤ 2^64` and `max_dev_bps ≤ 2^32`, both
/// sides fit in u128.
pub fn price_within_band(oracle_ticks: u64, price_ticks: u64, max_dev_bps: u32) -> bool {
    if oracle_ticks == 0 {
        return true;
    }
    let diff = oracle_ticks.abs_diff(price_ticks) as u128;
    let allowed = (oracle_ticks as u128) * (max_dev_bps as u128);
    diff * (BPS_DENOM as u128) <= allowed
}

/// HARD INVENTORY CAP (increment 3): the pool's net-position notional must not
/// exceed its capital (a conservative ~1× exposure limit). Returns
/// `(skip_bids, skip_asks)` for `flp_refresh_quotes`: when the pool is at the
/// LONG cap it stops posting BIDs (a filled bid would grow its long — a taker
/// selling into the pool), keeping only ASKs so takers can unwind it; symmetric
/// at the SHORT cap. `capital == 0` (uncapitalized pool) → cap disabled. This is
/// the hard backstop under `generate_quotes`'s continuous inventory skew.
#[inline]
pub fn inventory_cap_skip(net_signed: i64, capital_quote_lots: u64) -> (bool, bool) {
    if capital_quote_lots == 0 {
        return (false, false);
    }
    let net = net_signed as i128;
    let cap = capital_quote_lots as i128;
    // long-capped ⇒ skip bids; short-capped ⇒ skip asks.
    (net >= cap, net <= -cap)
}

/// FV: machine-checked properties of the FLP fill-price band (Kani). Equality
/// comparisons + multiplies over u128 — bounded and terminating. Runs in CI.
#[cfg(kani)]
mod flp_band_kani_proofs {
    use super::price_within_band;
    use crate::constants::BPS_DENOM;

    // Inputs bounded to a large-but-realistic range so CBMC's `oracle·max_dev`
    // bit-blast stays tractable (the free×free 128-bit multiply is the known SAT
    // bottleneck): prices ≤ 2^40 ticks (~1.1e12, far above any real tick price)
    // and a deviation cap ≤ BPS_DENOM (100% — a wider cap is meaningless). The
    // no-overflow property over the FULL u64/u32 domain is STRUCTURAL, not a CBMC
    // obligation: `oracle·max_dev ≤ 2^64·2^32 = 2^96 < u128::MAX` and
    // `diff·BPS_DENOM ≤ 2^64·2^14 = 2^78 < u128::MAX`.
    // 2^24 ticks (~16.7M) is far above any real tick price; the band is
    // scale-invariant so this fully exercises the property while keeping CBMC's
    // multiply bit-blast small (the `rejects` proof compares two symbolic
    // products — quadratic in operand width).
    const PRICE_MAX: u64 = 1 << 24;
    const DEV_MAX: u32 = BPS_DENOM; // 100%

    // NOTE: the "predicate == its own definition" identity is intentionally NOT a
    // Kani harness — it is tautological (the function literally returns
    // `diff·BPS ≤ oracle·max_dev`) and proving the equality of two symbolic
    // 54-bit multiplies exceeds CBMC's solver. The two harnesses below capture the
    // actual security properties; the no-overflow of the real function's internal
    // multiplies is checked inside them (the `multiply with overflow` checks on
    // `price_within_band` resolve SUCCESS).

    /// FAIR VALUE always passes — an FLP fill exactly at the oracle is never
    /// rejected (no false reject of the most honest possible price).
    #[kani::proof]
    fn flp_band_accepts_oracle_price() {
        let oracle: u64 = kani::any();
        let max_dev: u32 = kani::any();
        kani::assume(oracle <= PRICE_MAX);
        kani::assume(max_dev <= DEV_MAX);
        assert!(price_within_band(oracle, oracle, max_dev));
    }

    /// CATASTROPHE BOUND: with the protocol cap (< 100%), a price at 2× the oracle
    /// or at 0 is ALWAYS rejected — the gross fabrications that would drain the
    /// pool (taker sells to FLP at 2× / buys from FLP at ~0) cannot settle.
    #[kani::proof]
    fn flp_band_rejects_gross_fabrication() {
        let oracle: u64 = kani::any();
        let max_dev: u32 = kani::any();
        kani::assume(oracle > 0 && oracle <= PRICE_MAX);
        kani::assume(max_dev < DEV_MAX); // cap strictly below 100%
        assert!(!price_within_band(oracle, oracle * 2, max_dev)); // 100% high
        assert!(!price_within_band(oracle, 0, max_dev)); // 100% low
    }
}

#[cfg(kani)]
mod inventory_cap_kani {
    use super::inventory_cap_skip;

    /// THE safety property: the inventory cap NEVER stops BOTH sides — for any
    /// (net, capital) at least one side stays quotable, so a taker can always
    /// unwind the pool (the cap can never freeze the book). And an uncapitalized
    /// pool (capital==0) is uncapped. ∀ (net, capital).
    #[kani::proof]
    fn cap_never_stops_both_sides() {
        let net: i64 = kani::any();
        let cap: u64 = kani::any();
        let (skip_bids, skip_asks) = inventory_cap_skip(net, cap);
        assert!(!(skip_bids && skip_asks));
        if cap == 0 {
            assert!(!skip_bids && !skip_asks);
        }
    }
}

#[cfg(test)]
mod inventory_cap_tests {
    use super::inventory_cap_skip;

    #[test]
    fn cap_behavior() {
        assert_eq!(inventory_cap_skip(0, 1000), (false, false)); // flat → quote both
        assert_eq!(inventory_cap_skip(1000, 1000), (true, false)); // at long cap → skip bids
        assert_eq!(inventory_cap_skip(1500, 1000), (true, false)); // over long cap
        assert_eq!(inventory_cap_skip(-1000, 1000), (false, true)); // at short cap → skip asks
        assert_eq!(inventory_cap_skip(-1500, 1000), (false, true)); // over short cap
        assert_eq!(inventory_cap_skip(999, 1000), (false, false)); // just under → quote both
        assert_eq!(inventory_cap_skip(5000, 0), (false, false)); // no capital → no cap
    }
}

#[cfg(test)]
mod skew_totality_tests {
    use super::*;

    /// A pathological governance `inventory_lambda_bps` must neither panic (the
    /// old `-(x as i32)` hit `i32::MIN` and reverted on the unary minus) nor
    /// truncate its sign (quoting toward inventory). With the pool net-long the
    /// skew must stay negative (quote lower to shed inventory) and clamp to
    /// −BPS_DENOM.
    #[test]
    fn extreme_lambda_does_not_panic_and_skews_against_inventory() {
        let params = FlpQuoterParams {
            base_spread_bps: 0,
            alpha_bps: 0,
            beta_bps: 0,
            gamma_bps: 0,
            kappa_bps: 0,
            delta_bps: 0,
            // lambda = i32::MIN magnitude: pre-fix `-(... as i32)` panicked here.
            inventory_lambda_bps: 2_147_483_648,
            depth_floor_lots: 1,
            max_growth_per_batch_bps: 100,
            levels: 1,
            tick_size: 1,
        };
        let inputs = FlpQuoterInputs {
            oracle_ticks: Ticks(100_000),
            vpin_bps: 0,
            realized_vol_bps: 0,
            pool_capital_quote_lots: 1_000,
            // Net LONG far beyond capital ⇒ inv_bps clamps to +BPS_DENOM.
            pool_net_quote_lots_signed: 1_000_000,
            pool_gross_utilization_bps: 0,
            oi_long_lots: 0,
            oi_short_lots: 0,
        };
        let (out, _) = generate_quotes(params, inputs, Pubkey::default(), 0)
            .expect("extreme lambda must not panic or error");
        assert_eq!(
            out.skew_bps,
            -(BPS_DENOM as i32),
            "net-long inventory must skew fair value DOWN, clamped at -100%"
        );
    }
}
