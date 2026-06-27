//! verify_solvency — READ-ONLY maintenance-margin check on a single position.
//!
//! Builds the position + market snapshots and runs the ported (host-tested)
//! `risk::assess_margin` against a single ZERO-shock scenario, so the position's
//! equity is measured against its actual maintenance requirement (not merely
//! equity ≥ 0). Succeeds iff `available ≥ required`; errors `Custom(100)`
//! otherwise. Mutates NO state — the on-chain probe a keeper/client runs, and the
//! stepping-stone the (state-mutating) liquidation path will build on.
//!
//! Tiered MMR: if the optional `leverage_tiers` account is supplied (the market's
//! canonical PDA), the maintenance requirement is resolved at the position's
//! notional via the proven `tiered_mmr_bps` — so a large position is held to its
//! higher tier. Omit the account to assess at the flat base MMR.
//!
//! Cross-margin scope: `collateral` is the trader_state pool. (Isolated-bucket
//! handling is a follow-up, mirroring the still-TODO note in settle_funding.)
//!
//! accounts: [market, trader_state, position, (leverage_tiers — optional)]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::instructions::apply_fill::assert_position;
use crate::leverage_tiers::resolve_base_mmr;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::seeds::LEVERAGE_TIERS_SEED;
use crate::state::{
    Market, MarketLeverageTiers, Position, TraderState, LEVERAGE_TIERS_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, trader_state, position, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guards ──────────────────────────────────────────────────────────
    assert_market(market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_position(position, pid)?;

    // Optional leverage-tiers account: must be the market's canonical PDA
    // (owner + PDA + disc). Bound to this market below, inside the snapshot read.
    let tiers_account: Option<&AccountInfo> = match rest.first() {
        Some(a) => {
            assert_owned_by(a, pid)?;
            assert_pda(a, &[LEVERAGE_TIERS_SEED, &market.key()[..]], pid)?;
            assert_disc(a, &LEVERAGE_TIERS_DISC)?;
            Some(a)
        }
        None => None,
    };

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

        // Position notional (quote lots) = size · mark · tick — the same product
        // `assess_margin` forms, and the key `tiered_mmr_bps` reads.
        let notional = (p.size_lots as u128)
            .checked_mul(m.mark_price_ticks as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(m.tick_size as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // Resolve the maintenance base: flat market MMR, or the position's tier
        // if a leverage_tiers account was supplied (bound to this market).
        let base_mmr = match tiers_account {
            Some(a) => {
                let td = a.borrow_data_unchecked();
                let t = &*(td.as_ptr() as *const MarketLeverageTiers);
                if t.market != *market.key() {
                    return Err(ProgramError::InvalidArgument);
                }
                resolve_base_mmr(m.maintenance_margin_bps, t, notional)
            }
            None => m.maintenance_margin_bps,
        };

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
            // Tier-resolved (or flat) maintenance base. Concentration + OI-scaled
            // extras stay disabled in this probe (a follow-up once those params
            // are on the Market).
            maintenance_margin_bps: base_mmr,
            tick_size: m.tick_size,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
        (pos_snap, mkt_snap, ts.collateral_quote_lots)
    };

    // ── assess against a single zero-shock scenario so the MAINTENANCE
    //    requirement is actually evaluated (an empty scenario set would only
    //    check equity ≥ 0). `shocked_price(p, 0) == p`, so this prices the base
    //    maintenance margin at the current mark. ───────────────────────────
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[no_shock], collateral)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !assessment.is_healthy {
        // Below maintenance margin — the position is liquidatable.
        return Err(ProgramError::Custom(100));
    }
    Ok(())
}
