//! init_flp_per_market — create a market-scoped FLP-v3 exposure account, PDA
//! `[b"flp_per_market", market]`. Records the per-market pool-as-maker position
//! (created flat: `side = 255` = unset, all sizes/capital 0). The creator is the
//! authority. NO funds move; LP deposits/fills against it are later batches.
//!
//! Stricter than the anchor counterpart (which takes the market UncheckedAccount):
//! the market is verified program-owned + correct-disc, so an exposure account
//! can't be created against a fake market.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            exposure (PDA, w, uninit), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::FLP_PER_MARKET_SEED;
use crate::state::{FlpExposurePerMarketV3, Market, FLP_PER_MARKET_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const EXPOSURE_LEN: usize = core::mem::size_of::<FlpExposurePerMarketV3>();

/// Sentinel `side` for a flat (no-position) per-market FLP exposure.
const SIDE_UNSET: u8 = 255;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, exposure, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_market(market, program_id)?;
    // Admin-gate creation, and record the SEQUENCER as the per-market recorder.
    // `record_flp_fill_v3` lets `e.authority` move `realized_pnl`, and that now
    // drives NAV (the redemption price), so a permissionless creator could skim
    // LPs by faking PnL. Require the market authority to create the pool, and bind
    // the recorder to the market's sequencer (the trusted fill-settlement key).
    let (mkt_authority, mkt_sequencer) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.authority, m.sequencer)
    };
    if authority.key() != &mkt_authority {
        return Err(ProgramError::IllegalOwner);
    }
    assert_uninitialized(exposure)?;
    let bump = assert_pda(exposure, &[FLP_PER_MARKET_SEED, &market.key()[..]], program_id)?;

    let lamports = Rent::get()?.minimum_balance(EXPOSURE_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(FLP_PER_MARKET_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        exposure,
        system_program,
        lamports,
        EXPOSURE_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let e = &mut *(exposure.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut FlpExposurePerMarketV3);
        e.disc = FLP_PER_MARKET_V3_DISC;
        e.market = *market.key();
        e.authority = mkt_sequencer; // the trusted recorder (record_flp_fill_v3)
        e.size_lots = 0;
        e.entry_price_ticks = 0;
        e.total_capital_quote_lots = 0;
        e.realized_pnl = 0;
        e.lp_shares_outstanding = 0;
        e.bump = bump;
        e.side = SIDE_UNSET;
        e._reserved = [0u8; 22];
    }
    Ok(())
}
