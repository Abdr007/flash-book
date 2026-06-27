//! init_market_leverage_tiers — create a market's notional-banded MMR ladder,
//! PDA `[b"leverage_tiers", market]`. Authority-gated (only `market.authority`),
//! validated against the market's base MMR before it is written.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            leverage_tiers (PDA, w, uninit), system_program]
//! data: [tier_count u8][ (min_notional u64 LE)(mmr_bps u32 LE) ; tier_count ]

use crate::cpi::create_pda_account;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer, assert_uninitialized};
use crate::leverage_tiers::{parse_tiers, validate_tiers};
use crate::seeds::LEVERAGE_TIERS_SEED;
use crate::state::{
    LeverageTier, Market, MarketLeverageTiers, LEVERAGE_TIERS_DISC, MARKET_DISC, MAX_LEVERAGE_TIERS,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const TIERS_LEN: usize = core::mem::size_of::<MarketLeverageTiers>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, leverage_tiers, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── auth: real market of this program, signer is its admin ──────────
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

    // ── parse + validate the proposed ladder before creating anything ───
    let mut buf = [(0u64, 0u32); MAX_LEVERAGE_TIERS];
    let count = parse_tiers(data, &mut buf).map_err(|_| ProgramError::InvalidInstructionData)?;
    validate_tiers(base_mmr, &buf[..count]).map_err(|_| ProgramError::InvalidArgument)?;

    // ── create the PDA account ──────────────────────────────────────────
    assert_uninitialized(leverage_tiers)?;
    let bump = assert_pda(
        leverage_tiers,
        &[LEVERAGE_TIERS_SEED, &market.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(TIERS_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(LEVERAGE_TIERS_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        leverage_tiers,
        system_program,
        lamports,
        TIERS_LEN as u64,
        program_id,
        &signer,
    )?;

    // ── write the validated ladder ──────────────────────────────────────
    unsafe {
        let t = &mut *(leverage_tiers.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketLeverageTiers);
        t.disc = LEVERAGE_TIERS_DISC;
        t.market = *market.key();
        t.bump = bump;
        t.tier_count = count as u8;
        t._pad0 = [0u8; 6];
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
