//! initialize_side_accrual — create a market's side-accrual (ADL) state account,
//! PDA `[b"side_accrual", market]`. Both sides start at the identity multiplier
//! (`a = ADL_ONE`, k/f/b = 0, mode = Normal, epoch = 0) anchored at the supplied
//! initial price/slot. Market-authority gated. NO funds, NO book. The accrual /
//! ADL-execution consumers (matching-side) are later batches; this lays the
//! foundation that feeds the host-tested `side_accrual` math.
//!
//! accounts: [authority (signer, payer, w), market (program-owned, r),
//!            side_accrual (PDA, w, uninit), system_program]
//! data: [initial_price_ticks u64][initial_slot u64]   — 16 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::SIDE_ACCRUAL_SEED;
use crate::side_accrual::ADL_ONE;
use crate::state::{Market, MarketSideAccrual, SIDE_ACCRUAL_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const SIDE_ACCRUAL_LEN: usize = core::mem::size_of::<MarketSideAccrual>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, side_accrual, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let initial_price_ticks = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let initial_slot = u64::from_le_bytes(data[8..16].try_into().unwrap());

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
    assert_uninitialized(side_accrual)?;
    let bump = assert_pda(
        side_accrual,
        &[SIDE_ACCRUAL_SEED, &market.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(SIDE_ACCRUAL_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(SIDE_ACCRUAL_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        side_accrual,
        system_program,
        lamports,
        SIDE_ACCRUAL_LEN as u64,
        program_id,
        &signer,
    )?;

    // Identity-multiplier defaults for both sides.
    let adl_one = ADL_ONE.to_le_bytes();
    let zero = [0u8; 16];
    unsafe {
        let s = &mut *(side_accrual.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketSideAccrual);
        s.disc = SIDE_ACCRUAL_DISC;
        s.market = *market.key();
        s.bump = bump;
        s._pad0 = 0;
        s._reserved = [0u8; 64];

        s.long_slot_last = initial_slot;
        s.long_price_last = initial_price_ticks;
        s.short_slot_last = initial_slot;
        s.short_price_last = initial_price_ticks;
        s.long_epoch = 0;
        s.short_epoch = 0;
        s.long_mode = 0; // Normal
        s.short_mode = 0;

        s.long_a = adl_one;
        s.long_k = zero;
        s.long_f = zero;
        s.long_b = zero;
        s.short_a = adl_one;
        s.short_k = zero;
        s.short_f = zero;
        s.short_b = zero;
    }
    Ok(())
}
