//! verify_leverage_tiers — READ-ONLY consistency check on a market's leverage
//! (MMR) tier table. Re-runs the host-tested `leverage_tiers::validate_tiers`
//! over the STORED ladder against the market's base maintenance margin, so a
//! corrupted table (non-monotone, out-of-bounds rung, mmr below base) is caught.
//! Reverts `Custom(126)` on a breach. Mutates NO state.
//!
//! Port-addition (no standalone tier verify in anchor): same enforcing-probe
//! shape as the other `verify_*`, re-using the exact write-time validator.
//!
//! accounts: [market, leverage_tiers (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::leverage_tiers::validate_tiers;
use crate::seeds::LEVERAGE_TIERS_SEED;
use crate::state::{
    Market, MarketLeverageTiers, LEVERAGE_TIERS_DISC, MAX_LEVERAGE_TIERS,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, leverage_tiers, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(leverage_tiers, pid)?;
    assert_pda(leverage_tiers, &[LEVERAGE_TIERS_SEED, &market.key()[..]], pid)?;
    assert_disc(leverage_tiers, &LEVERAGE_TIERS_DISC)?;

    let base_mmr = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        m.maintenance_margin_bps
    };

    let mut buf = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
    let count = {
        let d = leverage_tiers.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const MarketLeverageTiers) };
        if &t.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let n = (t.tier_count as usize).min(MAX_LEVERAGE_TIERS);
        for (i, slot) in buf.iter_mut().enumerate().take(n) {
            *slot = (t.tiers[i].min_notional_quote_lots, t.tiers[i].mmr_bps);
        }
        n
    };

    validate_tiers(base_mmr, &buf[..count]).map_err(|_| ProgramError::Custom(126))?;
    Ok(())
}
