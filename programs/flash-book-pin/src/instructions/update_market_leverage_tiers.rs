//! update_market_leverage_tiers — replace a market's MMR ladder in place.
//! Authority-gated, re-validated against the market's base MMR. Binds the
//! existing tiers account to the market (it records its own `market` key).
//!
//! accounts: [authority (signer), market (program-owned, r),
//!            leverage_tiers (PDA, program-owned, w)]
//! data: [tier_count u8][ (min_notional u64 LE)(mmr_bps u32 LE) ; tier_count ]

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::leverage_tiers::{parse_tiers, validate_tiers};
use crate::seeds::LEVERAGE_TIERS_SEED;
use crate::state::{
    LeverageTier, Market, MarketLeverageTiers, LEVERAGE_TIERS_DISC, MARKET_DISC, MAX_LEVERAGE_TIERS,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, leverage_tiers, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── auth: real market, signer is its admin ──────────────────────────
    assert_signer(authority)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    let base_mmr = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
        m.maintenance_margin_bps
    };

    // ── the tiers account must be the canonical PDA bound to THIS market ─
    assert_owned_by(leverage_tiers, program_id)?;
    assert_pda(
        leverage_tiers,
        &[LEVERAGE_TIERS_SEED, &market.key()[..]],
        program_id,
    )?;
    assert_disc(leverage_tiers, &LEVERAGE_TIERS_DISC)?;
    {
        let d = leverage_tiers.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const MarketLeverageTiers) };
        if &t.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── parse + validate, then overwrite ────────────────────────────────
    let mut buf = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
    let count = parse_tiers(data, &mut buf).map_err(|_| ProgramError::InvalidInstructionData)?;
    validate_tiers(base_mmr, &buf[..count]).map_err(|_| ProgramError::InvalidArgument)?;

    unsafe {
        let t = &mut *(leverage_tiers.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketLeverageTiers);
        t.tier_count = count as u8;
        t.tiers = [LeverageTier { min_notional_quote_lots: 0, mmr_bps: 0, _pad: [0u8; 4] };
            MAX_LEVERAGE_TIERS];
        for (i, &(min_notional, mmr)) in buf[..count].iter().enumerate() {
            t.tiers[i] = LeverageTier {
                min_notional_quote_lots: min_notional,
                mmr_bps: mmr,
                _pad: [0u8; 4],
            };
        }
    }
    Ok(())
}
