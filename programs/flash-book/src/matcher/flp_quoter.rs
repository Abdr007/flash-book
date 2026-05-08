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
    pub alpha_bps: u32,           // VPIN coefficient
    pub beta_bps: u32,            // utilization coefficient
    pub gamma_bps: u32,           // OI imbalance coefficient
    pub kappa_bps: u32,           // depth amortization (Q/depth_floor)
    pub delta_bps: u32,           // realized-volatility coefficient
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
    let oi_total = inputs.oi_long_lots.checked_add(inputs.oi_short_lots).or_overflow()?;
    let oi_imb_bps: i32 = if oi_total > 0 {
        let diff: i128 = (inputs.oi_long_lots as i128) - (inputs.oi_short_lots as i128);
        ((diff * BPS_DENOM as i128) / oi_total as i128) as i32
    } else {
        0
    };

    // Inventory fraction in bps.
    let inv_bps: i32 = if inputs.pool_capital_quote_lots > 0 {
        let prod: i128 = (inputs.pool_net_quote_lots_signed as i128) * BPS_DENOM as i128;
        (prod / inputs.pool_capital_quote_lots as i128) as i32
    } else {
        0
    };

    // skew_bps = -lambda * inv_bps / BPS_DENOM (so lambda is in bps too).
    let skew_bps = -((params.inventory_lambda_bps as i64 * inv_bps as i64)
        / BPS_DENOM as i64) as i32;

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
        let oi_term = (params.gamma_bps as u64 * (oi_imb_bps.unsigned_abs() as u64)) / BPS_DENOM as u64;
        let depth_term = if params.depth_floor_lots > 0 {
            ((cum_size as u128) * (params.kappa_bps as u128) / (params.depth_floor_lots as u128)) as u64
        } else {
            0
        };
        let vol_term = (params.delta_bps as u64 * inputs.realized_vol_bps as u64) / BPS_DENOM as u64;
        let s_bps = (params.base_spread_bps as u64)
            .checked_add((params.alpha_bps as u64 * inputs.vpin_bps as u64) / BPS_DENOM as u64)
            .or_overflow()?
            .checked_add((params.beta_bps as u64 * inputs.pool_gross_utilization_bps as u64) / BPS_DENOM as u64)
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
