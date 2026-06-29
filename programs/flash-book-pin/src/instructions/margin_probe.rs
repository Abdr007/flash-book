//! Shared margin-probe snapshot builder for the READ-ONLY solvency instructions
//! (`verify_solvency`, `verify_stress_solvency`). Centralises the account guards,
//! the position↔trader↔market binding, tier-resolved maintenance base, and the
//! market's MMR surcharges — so the fund-critical snapshot logic lives in ONE
//! place and both probes assess identical inputs (only the stress scenario
//! differs).

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::instructions::apply_fill::assert_position;
use crate::leverage_tiers::resolve_base_mmr;
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{MarketSnapshot, PositionSnapshot};
use crate::seeds::LEVERAGE_TIERS_SEED;
use crate::state::{
    Market, MarketLeverageTiers, Position, TraderState, LEVERAGE_TIERS_DISC, TRADER_STATE_DISC,
};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

/// Validate the [market, trader_state, position, (leverage_tiers?)] account set
/// and build the owned snapshots + cross-margin collateral. Returns `Ok(None)`
/// for a flat position (trivially solvent — the caller should succeed). `rest` is
/// the slice after `position`; if non-empty its first element must be the
/// market's canonical `leverage_tiers` PDA, used to resolve the tier MMR.
pub(crate) fn build_snapshot(
    pid: &Pubkey,
    market: &AccountInfo,
    trader_state: &AccountInfo,
    position: &AccountInfo,
    rest: &[AccountInfo],
) -> Result<Option<(PositionSnapshot, MarketSnapshot, u64)>, ProgramError> {
    // ── guards ──────────────────────────────────────────────────────────
    assert_market(market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_position(position, pid)?;

    let tiers_account: Option<&AccountInfo> = match rest.first() {
        Some(a) => {
            assert_owned_by(a, pid)?;
            assert_pda(a, &[LEVERAGE_TIERS_SEED, &market.key()[..]], pid)?;
            assert_disc(a, &LEVERAGE_TIERS_DISC)?;
            Some(a)
        }
        None => None,
    };

    // ── read the validated accounts into owned, Copy snapshots ──────────
    unsafe {
        let m = &*(market.borrow_data_unchecked().as_ptr() as *const Market);
        let ts = &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState);
        let p = &*(position.borrow_data_unchecked().as_ptr() as *const Position);

        if p.size_lots == 0 {
            return Ok(None); // flat — trivially solvent
        }
        // Bind the position to THIS trader_state, not merely the wallet. Every one
        // of a wallet's trader_states (main + each sub-account) stores the same
        // `.trader = wallet`, so `p.trader == ts.trader` alone lets a wallet
        // substitute a tiny sub-account position into another trader_state's joint
        // solvency gate — the CRITICAL `partial_withdraw`/`sweep` collateral-theft
        // and the wrongful/under-margined `liquidate_portfolio`/`set_position_*`
        // walks. `(trader, sub_index)` bijectively identifies the trader_state
        // (the field-bound equivalent of Anchor's per-trader_state position PDA).
        // This single clause covers EVERY cross-portfolio walk, since they all
        // funnel position validation through `build_snapshot`.
        if p.trader != ts.trader || p.market != *market.key() || p.sub_index != ts.sub_index {
            return Err(ProgramError::InvalidArgument);
        }

        let notional = (p.size_lots as u128)
            .checked_mul(m.mark_price_ticks as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(m.tick_size as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;

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
            maintenance_margin_bps: base_mmr,
            tick_size: m.tick_size,
            concentration_threshold_lots: m.concentration_threshold_lots,
            concentration_extra_mmr_bps: m.concentration_extra_mmr_bps,
            side_oi_lots,
            oi_mmr_slope_bps_per_million_lots: m.oi_mmr_slope_bps_per_million_lots,
            oi_mmr_max_extra_bps: m.oi_mmr_max_extra_bps,
        };
        Ok(Some((pos_snap, mkt_snap, ts.collateral_quote_lots)))
    }
}
