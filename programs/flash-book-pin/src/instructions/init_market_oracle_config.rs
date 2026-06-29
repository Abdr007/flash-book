//! init_market_oracle_config — create a market's oracle config account, PDA
//! `[b"oracle_config", market]`. Market-authority gated, validated. Records the
//! Pyth feed id + freshness bounds; `source` is set to Pyth. The Pyth-pull
//! consumer (`update_oracle_from_pyth`) is a later batch — the port's live mark
//! path is still the trusted, sequencer-gated `update_oracle`.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            oracle_config (PDA, w, uninit), system_program]
//! data: [pyth_price_feed_id [u8;32]][max_staleness_seconds u32]
//!       [max_confidence_bps u32][tick_decimals i8]   — 41 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::ORACLE_CONFIG_SEED;
use crate::state::{
    Market, MarketOracleConfig, ORACLE_CONFIG_DISC, ORACLE_SOURCE_PYTH,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const ORACLE_CONFIG_LEN: usize = core::mem::size_of::<MarketOracleConfig>();
/// Confidence cap ceiling (bps) — mirrors the anchor bound.
const MAX_CONFIDENCE_BPS: u32 = 1_000;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, oracle_config, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 41 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut pyth_price_feed_id = [0u8; 32];
    pyth_price_feed_id.copy_from_slice(&data[0..32]);
    let max_staleness_seconds = u32::from_le_bytes(data[32..36].try_into().unwrap());
    let max_confidence_bps = u32::from_le_bytes(data[36..40].try_into().unwrap());
    let tick_decimals = data[40] as i8;
    // Optional quorum dispersion cap appended after the fixed 41 bytes; 0 / absent
    // = dispersion gate off (backward-compatible).
    let max_dispersion_bps = if data.len() >= 45 {
        u32::from_le_bytes(data[41..45].try_into().unwrap())
    } else {
        0
    };

    // Validation (anchor parity).
    if max_staleness_seconds == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if max_confidence_bps == 0 || max_confidence_bps > MAX_CONFIDENCE_BPS {
        return Err(ProgramError::InvalidArgument);
    }

    // ── auth: market authority ──────────────────────────────────────────
    assert_signer(authority)?;
    assert_market(market, program_id)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // ── create the PDA ──────────────────────────────────────────────────
    assert_uninitialized(oracle_config)?;
    let bump = assert_pda(
        oracle_config,
        &[ORACLE_CONFIG_SEED, &market.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(ORACLE_CONFIG_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(ORACLE_CONFIG_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        oracle_config,
        system_program,
        lamports,
        ORACLE_CONFIG_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let c = &mut *(oracle_config.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketOracleConfig);
        c.disc = ORACLE_CONFIG_DISC;
        c.market = *market.key();
        c.pyth_price_feed_id = pyth_price_feed_id;
        c.max_staleness_seconds = max_staleness_seconds;
        c.max_confidence_bps = max_confidence_bps;
        c.tick_decimals = tick_decimals;
        c.source = ORACLE_SOURCE_PYTH;
        c.bump = bump;
        c._pad0 = 0;
        c.max_dispersion_bps = max_dispersion_bps;
    }
    Ok(())
}
