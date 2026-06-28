//! init_position_liquidation_state — create a position's liquidation-state
//! account, PDA `[b"position_liq", market, position]`, holding the timestamps
//! `liquidate_position_v2` needs that don't fit in the full 128-byte `Position`
//! (`unhealthy_since_slot` for the Dutch-auction reward, `last_liquidated_at_slot`
//! for the re-liquidation cooldown). Created empty (all zero). NO funds, NO book.
//! Anyone may pay to create it. Mirrors `init_position_haircut_state`.
//!
//! accounts: [payer (signer, w), position (program-owned, r),
//!            position_liq (PDA, w, uninit), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::seeds::POSITION_LIQ_STATE_SEED;
use crate::state::{Position, PositionLiquidationState, POSITION_LIQ_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const POS_LIQ_LEN: usize = core::mem::size_of::<PositionLiquidationState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, position, position_liq, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(payer)?;
    assert_position(position, program_id)?;

    // The position's market — drives the PDA seed.
    let position_market = {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const Position) };
        p.market
    };

    let bump = assert_pda(
        position_liq,
        &[POSITION_LIQ_STATE_SEED, &position_market[..], &position.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(POS_LIQ_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(POSITION_LIQ_STATE_SEED),
        Seed::from(&position_market[..]),
        Seed::from(&position.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        payer,
        position_liq,
        system_program,
        lamports,
        POS_LIQ_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let s = &mut *(position_liq.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut PositionLiquidationState);
        s.disc = POSITION_LIQ_STATE_DISC;
        s.market = position_market;
        s.position = *position.key();
        s.unhealthy_since_slot = 0;
        s.last_liquidated_at_slot = 0;
        s.bump = bump;
        s._pad0 = [0u8; 7];
        s._reserved = [0u8; 24];
    }
    Ok(())
}
