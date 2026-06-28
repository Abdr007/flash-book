//! liquidate_portfolio_v2 — liquidate one leg (the EXECUTION position) of a
//! CROSS trader whose WHOLE portfolio is underwater. The health gate assesses
//! the trader's complete cross position set against the cross pool (H2 coverage),
//! so a hedged trader is never wrongfully liquidated on a single adverse leg —
//! the path `liquidate_position_v2` routes multi-leg cross traders to. On an
//! unhealthy verdict it injects a forced-liquidation order (order_type 3) for the
//! FULL execution-leg size at the synthetic penalty price; the close settles when
//! the matcher fills it. NO reward, NO position_liq stamp (those are the
//! single-position path's). Faithful port of the Anchor `liquidate_portfolio_v2`,
//! MARK-only (pin has no separate oracle).
//!
//! H3: refuse to stack a DUPLICATE — if an order_type==3 order for this trader
//! already rests on the close side, the leg is already being liquidated.
//!
//! accounts: [caller(signer), execution_market, execution_market_book(PDA,w),
//!            trader_state, execution_position, <market, position> × (open−1)]
//! data: (none)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::hypertree::DataIndex;
use crate::instructions::apply_fill::assert_position;
use crate::instructions::margin_probe::build_snapshot;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::state::{Market, Position, Pubkey as PubkeyBytes, TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

const MAX_PORTFOLIO: usize = 8;

#[inline]
fn zero_position() -> PositionSnapshot {
    PositionSnapshot {
        market: [0u8; 32], side: Side::Long, size_lots: 0, entry_price: Ticks(0),
        cum_funding_index_at_entry: 0, collateral_quote_lots: 0,
    }
}
#[inline]
fn zero_market() -> MarketSnapshot {
    MarketSnapshot {
        market: [0u8; 32], mark_price: Ticks(0), cum_funding_index: 0, maintenance_margin_bps: 0,
        tick_size: 0, concentration_threshold_lots: 0, concentration_extra_mmr_bps: 0,
        side_oi_lots: 0, oi_mmr_slope_bps_per_million_lots: 0, oi_mmr_max_extra_bps: 0,
    }
}

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, exec_market, exec_book, trader_state, exec_position, siblings @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_market(exec_market, pid)?;
    assert_market_book(exec_book, exec_market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_position(exec_position, pid)?;

    let exec_market_key = *exec_market.key();
    let (ex_trader, ex_market, ex_side, ex_size) = {
        let p = unsafe { &*(exec_position.borrow_data_unchecked().as_ptr() as *const Position) };
        (p.trader, p.market, p.side, p.size_lots)
    };
    let (ts_trader, ts_collat, ts_open, ts_sub) = {
        let ts = unsafe { &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState) };
        (ts.trader, ts.collateral_quote_lots, ts.open_positions, ts.sub_index)
    };
    let (mark, penalty_bps) = {
        let m = unsafe { &*(exec_market.borrow_data_unchecked().as_ptr() as *const Market) };
        (m.mark_price_ticks, m.liq_penalty_bps)
    };

    if ex_size == 0 || ex_trader != ts_trader || ex_market != exec_market_key {
        return Err(ProgramError::InvalidArgument);
    }
    if ex_side > 1 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── H2 coverage: exec leg + exactly (open − 1) sibling pairs ────────
    if siblings.len() % 2 != 0 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let sib_count = siblings.len() / 2;
    if sib_count != (ts_open.saturating_sub(1)) as usize {
        return Err(ProgramError::InvalidArgument);
    }
    if sib_count + 1 > MAX_PORTFOLIO {
        return Err(ProgramError::InvalidInstructionData);
    }

    // ── build the full cross portfolio (exec leg first), dedup markets ──
    let mut positions = [zero_position(); MAX_PORTFOLIO];
    let mut markets = [zero_market(); MAX_PORTFOLIO];
    let mut seen: [PubkeyBytes; MAX_PORTFOLIO] = [[0u8; 32]; MAX_PORTFOLIO];
    let mut n = 0usize;

    let Some((ex_pos_snap, ex_mkt_snap, _c)) =
        build_snapshot(pid, exec_market, trader_state, exec_position, &[])?
    else {
        return Err(ProgramError::InvalidArgument);
    };
    positions[n] = ex_pos_snap;
    markets[n] = ex_mkt_snap;
    seen[n] = exec_market_key;
    n += 1;

    for i in 0..sib_count {
        let m_ai = &siblings[2 * i];
        let p_ai = &siblings[2 * i + 1];
        let Some((pos_snap, mkt_snap, _c)) = build_snapshot(pid, m_ai, trader_state, p_ai, &[])?
        else {
            return Err(ProgramError::InvalidArgument);
        };
        if seen[..n].iter().any(|k| k == m_ai.key()) {
            return Err(ProgramError::InvalidArgument); // duplicate (incl. exec market)
        }
        seen[n] = *m_ai.key();
        positions[n] = pos_snap;
        markets[n] = mkt_snap;
        n += 1;
    }

    // ── full-portfolio health gate: must be UNHEALTHY ───────────────────
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&positions[..n], &markets[..n], &[no_shock], ts_collat)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if assessment.is_healthy {
        return Err(ProgramError::InvalidArgument); // NotLiquidatable
    }

    // ── synthetic close price + inject the full-size liquidation order ──
    let close_side = 1 - ex_side;
    let limit = crate::liquidation::liquidation_penalty_price(close_side, mark, penalty_bps);
    if limit == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    let now = Clock::get()?.slot;

    unsafe {
        let book_data = exec_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        if &handle.header.market_pubkey != exec_market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        // H3: a forced-liquidation order (type 3) for this trader already resting
        // on the close side ⇒ already being liquidated; refuse to stack.
        let mut dup = false;
        {
            let mut scan = |_idx: DataIndex, o: &RestingOrderV2| -> bool {
                if o.order_type == 3 && o.trader == ex_trader {
                    dup = true;
                    return false;
                }
                true
            };
            if close_side == 0 {
                handle.for_each_bid_best_first(&mut scan);
            } else {
                handle.for_each_ask_best_first(&mut scan);
            }
        }
        if dup {
            return Err(ProgramError::Custom(140)); // already being liquidated
        }
        let seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = seq;
        let side_is_bid = close_side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(limit, seq, side_is_bid),
            seq,
            price_ticks: limit,
            size_lots: ex_size,
            expires_at_slot: 0,
            trader: ex_trader,
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: close_side,
            order_type: 3,
            flags: 0,
            sub_index: ts_sub,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }
    Ok(())
}
