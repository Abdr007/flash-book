//! init_position_haircut_state — create a position's haircut (positive-PnL
//! warmup) state account, PDA `[b"position_haircut", market, position]`. Gated on
//! the market's haircut engine already being enabled (the market haircut_state
//! PDA must exist + bind to the position's market). Created empty. NO funds, NO
//! book. Anyone may pay to create it.
//!
//! accounts: [payer (signer, w), position (program-owned, r),
//!            market_haircut (PDA, program-owned, r),
//!            position_haircut (PDA, w, uninit), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::seeds::{HAIRCUT_SEED, POSITION_HAIRCUT_SEED};
use crate::state::{
    MarketHaircutState, Position, PositionHaircutState, HAIRCUT_STATE_DISC, POSITION_HAIRCUT_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const POS_HAIRCUT_LEN: usize = core::mem::size_of::<PositionHaircutState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, position, market_haircut, position_haircut, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(payer)?;
    assert_position(position, program_id)?;

    // The position's market — drives both PDA seeds below.
    let position_market = {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const Position) };
        p.market
    };

    // Gate: the market's haircut engine must be enabled (its state PDA exists +
    // is bound to this position's market).
    assert_owned_by(market_haircut, program_id)?;
    assert_pda(market_haircut, &[HAIRCUT_SEED, &position_market[..]], program_id)?;
    assert_disc(market_haircut, &HAIRCUT_STATE_DISC)?;
    {
        let d = market_haircut.try_borrow_data()?;
        let mh = unsafe { &*(d.as_ptr() as *const MarketHaircutState) };
        if mh.market != position_market {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // Create the per-position state PDA.
    let bump = assert_pda(
        position_haircut,
        &[POSITION_HAIRCUT_SEED, &position_market[..], &position.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(POS_HAIRCUT_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(POSITION_HAIRCUT_SEED),
        Seed::from(&position_market[..]),
        Seed::from(&position.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        payer,
        position_haircut,
        system_program,
        lamports,
        POS_HAIRCUT_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let s = &mut *(position_haircut.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut PositionHaircutState);
        s.disc = POSITION_HAIRCUT_DISC;
        s.market = position_market;
        s.position = *position.key();
        s.released_reserve_quote_lots = 0;
        s.released_attached_at_slot = 0;
        s.matured_pos_quote_lots = 0;
        s.original_reserve_at_attach = 0;
        s.bump = bump;
        s._pad0 = [0u8; 7];
        s._reserved = [0u8; 24];
    }
    Ok(())
}
