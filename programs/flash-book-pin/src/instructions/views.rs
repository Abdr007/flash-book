//! view_* — read-only "view" instructions, ported for anchor parity (134/134).
//!
//! In Anchor these emit events that off-chain UIs decode via the IDL. pin has no
//! Anchor event/IDL machinery, so each computes the values its inputs make
//! available in pin's (leaner, mark-only) model and emits them via `sol_log_data`
//! with a documented `[tag][packed LE fields]` layout. They change NO state.
//!
//! HONEST LIMITATIONS — pin's reduced account schema omits several fields the
//! anchor views read, so those are emitted as `0` and flagged per-view:
//!   * no `Market.oracle_price_ticks` (mark-only) → funding premium is 0;
//!   * no `TraderState.volume_30d_*` → effective volume is 0 (base tier);
//!   * the FLP quoter / stress-lattice walks are not re-run (summary only).

use crate::book::MarketBookHandle;
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by};
use crate::state::{Market, TraderState, MARKET_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, log::sol_log_data, program_error::ProgramError, pubkey::Pubkey,
    ProgramResult,
};

const TAG_PREDICTED_FUNDING: u8 = 0xF0;
const TAG_TRADER_TIER: u8 = 0xF1;
const TAG_BOOK_DEPTH: u8 = 0xF2;
const TAG_QUOTE_LADDER: u8 = 0xF3;
const TAG_PORTFOLIO_RISK: u8 = 0xF4;

/// view_predicted_funding — emits mark + cum-funding. premium/rate are 0: pin is
/// mark-only (the sequencer sets mark directly; there is no oracle-mark spread).
/// accounts: [market (program-owned)]
pub fn predicted_funding(pid: &Pubkey, accounts: &[AccountInfo], _d: &[u8]) -> ProgramResult {
    let [market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    let (mark, cum) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.mark_price_ticks, m.cum_funding_index)
    };
    // [tag][market(32)][mark u64][premium=0 i64][rate=0 i64][cum_funding i128]
    let mut buf = [0u8; 1 + 32 + 8 + 8 + 8 + 16];
    buf[0] = TAG_PREDICTED_FUNDING;
    buf[1..33].copy_from_slice(market.key());
    buf[33..41].copy_from_slice(&mark.to_le_bytes());
    // premium (41..49) + rate (49..57) stay 0 — degenerate in pin's mark-only model.
    buf[57..73].copy_from_slice(&cum);
    sol_log_data(&[&buf]);
    Ok(())
}

/// view_trader_effective_tier — emits the trader's fee discount. tier/volume are
/// 0: pin's TraderState has no 30-day volume accumulator (base tier).
/// accounts: [trader_state (program-owned)]
pub fn trader_effective_tier(pid: &Pubkey, accounts: &[AccountInfo], _d: &[u8]) -> ProgramResult {
    let [trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    let (trader, fee_discount_bps) = {
        let d = trader_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        (s.trader, s.fee_discount_bps)
    };
    // [tag][trader(32)][tier_index=0 u8][effective_volume=0 u64][fee_discount_bps u32]
    let mut buf = [0u8; 1 + 32 + 1 + 8 + 4];
    buf[0] = TAG_TRADER_TIER;
    buf[1..33].copy_from_slice(&trader);
    // tier_index (33) + volume (34..42) stay 0 — no volume tracking in pin.
    buf[42..46].copy_from_slice(&fee_discount_bps.to_le_bytes());
    sol_log_data(&[&buf]);
    Ok(())
}

/// view_book_depth_v2 — emits the live resting-order count (real pin data). The
/// per-level ladder walk is summarized to the active count here.
/// accounts: [market (PDA), market_book (PDA)]
pub fn book_depth(pid: &Pubkey, accounts: &[AccountInfo], _d: &[u8]) -> ProgramResult {
    let [market, market_book, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;
    let active = unsafe {
        let bd = market_book.borrow_mut_data_unchecked();
        MarketBookHandle::from_account_data(bd)?.header.total_orders_active
    };
    // [tag][market(32)][total_orders_active u32]
    let mut buf = [0u8; 1 + 32 + 4];
    buf[0] = TAG_BOOK_DEPTH;
    buf[1..33].copy_from_slice(market.key());
    buf[33..37].copy_from_slice(&active.to_le_bytes());
    sol_log_data(&[&buf]);
    Ok(())
}

/// view_quote_ladder — emits mark + tick (real); levels=0: the FLP quoter's
/// `generate_quotes` is not re-run in this view.
/// accounts: [market (program-owned)]
pub fn quote_ladder(pid: &Pubkey, accounts: &[AccountInfo], _d: &[u8]) -> ProgramResult {
    let [market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    let (mark, tick) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.mark_price_ticks, m.tick_size)
    };
    // [tag][market(32)][mark u64][tick u64][levels=0 u32]
    let mut buf = [0u8; 1 + 32 + 8 + 8 + 4];
    buf[0] = TAG_QUOTE_LADDER;
    buf[1..33].copy_from_slice(market.key());
    buf[33..41].copy_from_slice(&mark.to_le_bytes());
    buf[41..49].copy_from_slice(&tick.to_le_bytes());
    sol_log_data(&[&buf]);
    Ok(())
}

/// view_portfolio_risk — emits the trader's collateral + open-position count
/// (real). The full stress-lattice assessment (a remaining_accounts walk) is not
/// re-run here; the risk score is left 0.
/// accounts: [trader_state (program-owned)]
pub fn portfolio_risk(pid: &Pubkey, accounts: &[AccountInfo], _d: &[u8]) -> ProgramResult {
    let [trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    let (trader, collateral, open) = {
        let d = trader_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        (s.trader, s.collateral_quote_lots, s.open_positions)
    };
    // [tag][trader(32)][collateral u64][open_positions u8][risk_score=0 u32]
    let mut buf = [0u8; 1 + 32 + 8 + 1 + 4];
    buf[0] = TAG_PORTFOLIO_RISK;
    buf[1..33].copy_from_slice(&trader);
    buf[33..41].copy_from_slice(&collateral.to_le_bytes());
    buf[41] = open;
    sol_log_data(&[&buf]);
    Ok(())
}
