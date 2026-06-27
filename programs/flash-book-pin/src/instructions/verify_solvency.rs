//! verify_solvency — READ-ONLY maintenance-margin check on a single position.
//!
//! Builds the position + market snapshots and runs the ported (host-tested)
//! `risk::assess_margin` at the base (no-stress) maintenance requirement.
//! Succeeds iff the position is solvent (equity ≥ required); errors otherwise.
//! Mutates NO state — it's the on-chain solvency probe a keeper/client runs, and
//! the stepping-stone the (state-mutating) liquidation path will build on.
//!
//! Cross-margin scope: `collateral` is the trader_state pool. (Isolated-bucket
//! handling is a follow-up, mirroring the still-TODO note in settle_funding.)
//!
//! accounts: [market, trader_state, position]

use crate::guard::{assert_disc, assert_market, assert_owned_by};
use crate::instructions::apply_fill::assert_position;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot};
use crate::state::{Market, Position, TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guards ──────────────────────────────────────────────────────────
    assert_market(market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_position(position, pid)?;

    // ── build the (owned, Copy) snapshots from the validated accounts ───
    let (pos_snap, mkt_snap, collateral) = unsafe {
        let m = &*(market.borrow_data_unchecked().as_ptr() as *const Market);
        let ts = &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState);
        let p = &*(position.borrow_data_unchecked().as_ptr() as *const Position);

        // A flat position is trivially solvent.
        if p.size_lots == 0 {
            return Ok(());
        }
        // Bind the position to THIS trader_state + market.
        if p.trader != ts.trader || p.market != *market.key() {
            return Err(ProgramError::InvalidArgument);
        }

        let side = if p.side == 0 { Side::Long } else { Side::Short };
        let pos_snap = PositionSnapshot {
            market: p.market,
            side,
            size_lots: p.size_lots,
            entry_price: Ticks(p.entry_price_ticks),
            cum_funding_index_at_entry: p.cum_funding(),
            collateral_quote_lots: p.collateral_quote_lots,
        };
        let side_oi_lots = if p.side == 0 { m.long_oi_lots } else { m.short_oi_lots };
        let mkt_snap = MarketSnapshot {
            market: *market.key(),
            mark_price: Ticks(m.mark_price_ticks),
            cum_funding_index: m.cum_funding(),
            maintenance_margin_bps: m.maintenance_margin_bps,
            tick_size: m.tick_size,
            // Concentration + OI-scaled MMR extras are disabled in this minimal
            // probe (a follow-up once those params are on the Market).
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
        (pos_snap, mkt_snap, ts.collateral_quote_lots)
    };

    // ── assess (no-stress base maintenance) ─────────────────────────────
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        // Below maintenance margin — the position is liquidatable.
        return Err(ProgramError::Custom(100));
    }
    Ok(())
}
