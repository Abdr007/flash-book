//! initialize_haircut_state — create a market's haircut (positive-PnL warmup)
//! state account, PDA `[b"haircut", market]`, and ENABLE the haircut engine on
//! the market (sticky flag). Market-authority gated; the warmup window is
//! validated by the host-tested `haircut::validate_market_params`. NO funds, NO
//! book. The settlement consumer that requires the haircut accounts is a later
//! batch.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, w),
//!            haircut_state (PDA, w, uninit), system_program]
//! data: [h_min_slots u64][h_max_slots u64][initial_residual_quote_lots u128 LE]
//!       — 32 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::haircut::{validate_market_params, H_DENOM};
use crate::seeds::HAIRCUT_SEED;
use crate::state::{Market, MarketHaircutState, HAIRCUT_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const HAIRCUT_LEN: usize = core::mem::size_of::<MarketHaircutState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, haircut_state, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let h_min_slots = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let h_max_slots = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let mut initial_residual = [0u8; 16];
    initial_residual.copy_from_slice(&data[16..32]);

    // Validate the warmup window (host-tested haircut math).
    validate_market_params(h_min_slots, h_max_slots).map_err(|_| ProgramError::InvalidArgument)?;

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
    assert_uninitialized(haircut_state)?;
    let bump = assert_pda(haircut_state, &[HAIRCUT_SEED, &market.key()[..]], program_id)?;
    let lamports = Rent::get()?.minimum_balance(HAIRCUT_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(HAIRCUT_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        haircut_state,
        system_program,
        lamports,
        HAIRCUT_LEN as u64,
        program_id,
        &signer,
    )?;

    let now = Clock::get()?.slot;
    let zero = [0u8; 16];
    unsafe {
        let s = &mut *(haircut_state.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketHaircutState);
        s.disc = HAIRCUT_STATE_DISC;
        s.market = *market.key();
        s.bump = bump;
        s._pad0 = [0u8; 7];
        s.h_min_slots = h_min_slots;
        s.h_max_slots = h_max_slots;
        s.h_scaled_cached = H_DENOM as u64;
        s.h_cached_at_slot = now;
        s.residual_quote_lots = initial_residual;
        s.matured_pos_total_quote_lots = zero;
        s.realized_loss_total_quote_lots = zero;
        s.dust_accrued_quote_lots = zero;
        s._reserved = [0u8; 64];
    }

    // Enable the haircut engine on the market (sticky).
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.haircut_enabled = 1;
    }
    Ok(())
}
