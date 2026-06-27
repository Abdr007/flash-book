//! set_envelope_config — set (or re-set) a market's envelope (price-band /
//! risk-invariant) parameters. Market-authority gated. The 7 params are PROVEN by
//! the host-tested `envelope::prove_envelope` before anything is written, so a
//! config that violates the envelope invariant can never be stored. Init-or-update:
//! the first call creates the PDA `[b"envelope", market]`; later calls overwrite
//! the params and bump the version. NO funds, NO book.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            envelope_config (PDA, w; uninit on first call), system_program]
//! data: [max_price_move_bps_per_slot u32][max_accrual_dt_slots u64]
//!       [max_abs_funding_e9_per_slot i64][maintenance_bps u32]
//!       [liquidation_fee_bps u32][min_liquidation_abs_lots u64]
//!       [min_nonzero_mm_req_lots u64]   — 44 bytes

use crate::cpi::create_pda_account;
use crate::envelope::{prove_envelope, EnvelopeParams};
use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::guard::assert_signer;
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{Market, MarketEnvelopeConfig, ENVELOPE_CONFIG_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const ENVELOPE_LEN: usize = core::mem::size_of::<MarketEnvelopeConfig>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, envelope_config, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 44 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let params = EnvelopeParams {
        max_price_move_bps_per_slot: u32::from_le_bytes(data[0..4].try_into().unwrap()),
        max_accrual_dt_slots: u64::from_le_bytes(data[4..12].try_into().unwrap()),
        max_abs_funding_e9_per_slot: i64::from_le_bytes(data[12..20].try_into().unwrap()),
        maintenance_bps: u32::from_le_bytes(data[20..24].try_into().unwrap()),
        liquidation_fee_bps: u32::from_le_bytes(data[24..28].try_into().unwrap()),
        min_liquidation_abs_lots: u64::from_le_bytes(data[28..36].try_into().unwrap()),
        min_nonzero_mm_req_lots: u64::from_le_bytes(data[36..44].try_into().unwrap()),
    };
    // Prove the envelope invariant BEFORE writing anything.
    prove_envelope(&params).map_err(|_| ProgramError::InvalidArgument)?;

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

    let bump = assert_pda(
        envelope_config,
        &[ENVELOPE_CONFIG_SEED, &market.key()[..]],
        program_id,
    )?;
    let now = Clock::get()?.slot;

    // ── init-or-update ──────────────────────────────────────────────────
    if envelope_config.data_len() == 0 {
        // First call: create + initialize (version 1, runtime gate fields 0).
        let lamports = Rent::get()?.minimum_balance(ENVELOPE_LEN);
        let bump_arr = [bump];
        let seeds = [
            Seed::from(ENVELOPE_CONFIG_SEED),
            Seed::from(&market.key()[..]),
            Seed::from(&bump_arr[..]),
        ];
        let signer = [Signer::from(&seeds[..])];
        create_pda_account(
            authority,
            envelope_config,
            system_program,
            lamports,
            ENVELOPE_LEN as u64,
            program_id,
            &signer,
        )?;
        unsafe {
            let c = &mut *(envelope_config.borrow_mut_data_unchecked().as_mut_ptr()
                as *mut MarketEnvelopeConfig);
            c.disc = ENVELOPE_CONFIG_DISC;
            c.market = *market.key();
            c.bump = bump;
            c._pad = [0u8; 7];
            c._reserved = [0u8; 32];
            c.last_observed_slot = 0;
            c.last_observed_price_ticks = 0;
            c.gate_passes = 0;
            c.gate_rejects = 0;
            c.version = 1;
            write_params(c, &params, now);
        }
    } else {
        // Subsequent call: must be the genuine, market-bound config; overwrite.
        assert_owned_by(envelope_config, program_id)?;
        assert_disc(envelope_config, &ENVELOPE_CONFIG_DISC)?;
        unsafe {
            let c = &mut *(envelope_config.borrow_mut_data_unchecked().as_mut_ptr()
                as *mut MarketEnvelopeConfig);
            if &c.market != market.key() {
                return Err(ProgramError::InvalidArgument);
            }
            c.version = c.version.saturating_add(1);
            write_params(c, &params, now);
        }
    }
    Ok(())
}

#[inline(always)]
unsafe fn write_params(c: &mut MarketEnvelopeConfig, p: &EnvelopeParams, now: u64) {
    c.max_price_move_bps_per_slot = p.max_price_move_bps_per_slot;
    c.max_accrual_dt_slots = p.max_accrual_dt_slots;
    c.max_abs_funding_e9_per_slot = p.max_abs_funding_e9_per_slot;
    c.maintenance_bps = p.maintenance_bps;
    c.liquidation_fee_bps = p.liquidation_fee_bps;
    c.min_liquidation_abs_lots = p.min_liquidation_abs_lots;
    c.min_nonzero_mm_req_lots = p.min_nonzero_mm_req_lots;
    c.last_proven_at_slot = now;
}
