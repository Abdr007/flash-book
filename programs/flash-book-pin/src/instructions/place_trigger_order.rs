//! place_trigger_order — create a v3 conditional (trigger/stop) order account,
//! PDA `[b"trigger_v3", market, trader, trigger_id]`. Validated against the
//! market's lot/tick rules; records the trader's intent. NO funds move and NO
//! book is touched at placement — the order sits until a separate (matching) exec
//! path fires it.
//!
//! accounts: [trader (signer, payer, w), market (program-owned, r),
//!            trigger_order (PDA, w, uninit), system_program]
//! data: [trigger_id u8][side u8][kind u8][flags u8][sub_index u8]
//!       [size_lots u64][trigger_price u64][limit_price u64]
//!       [expires_at_slot u64][acceptable_price u64]   — 45 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TRIGGER_ORDER_SEED;
use crate::state::{Market, TriggerOrderV3, TRIGGER_ORDER_V3_DISC};
use crate::trigger_order::validate_trigger_params;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const TRIGGER_LEN: usize = core::mem::size_of::<TriggerOrderV3>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, market, trigger_order, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 45 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let trigger_id = data[0];
    let side = data[1];
    let kind = data[2];
    let flags = data[3];
    let sub_index = data[4];
    let size_lots = u64::from_le_bytes(data[5..13].try_into().unwrap());
    let trigger_price_ticks = u64::from_le_bytes(data[13..21].try_into().unwrap());
    let limit_price_ticks = u64::from_le_bytes(data[21..29].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[29..37].try_into().unwrap());
    let acceptable_price_ticks = u64::from_le_bytes(data[37..45].try_into().unwrap());

    assert_signer(trader)?;
    assert_market(market, program_id)?;

    // Validate against this market's lot/tick rules.
    let (min_base_lots, tick_size) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.min_base_lots, m.tick_size)
    };
    validate_trigger_params(
        side,
        kind,
        size_lots,
        trigger_price_ticks,
        limit_price_ticks,
        acceptable_price_ticks,
        min_base_lots,
        tick_size,
    )
    .map_err(|_| ProgramError::InvalidArgument)?;

    // Create the PDA (unique per (market, trader, trigger_id)).
    assert_uninitialized(trigger_order)?;
    let id_arr = [trigger_id];
    let bump = assert_pda(
        trigger_order,
        &[
            TRIGGER_ORDER_SEED,
            &market.key()[..],
            &trader.key()[..],
            &id_arr[..],
        ],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(TRIGGER_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TRIGGER_ORDER_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&trader.key()[..]),
        Seed::from(&id_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        trader,
        trigger_order,
        system_program,
        lamports,
        TRIGGER_LEN as u64,
        program_id,
        &signer,
    )?;

    let created_at_slot = Clock::get()?.slot;
    unsafe {
        let t = &mut *(trigger_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut TriggerOrderV3);
        t.disc = TRIGGER_ORDER_V3_DISC;
        t.trader = *trader.key();
        t.market = *market.key();
        t.size_lots = size_lots;
        t.trigger_price_ticks = trigger_price_ticks;
        t.limit_price_ticks = limit_price_ticks;
        t.created_at_slot = created_at_slot;
        t.expires_at_slot = expires_at_slot;
        t.acceptable_price_ticks = acceptable_price_ticks;
        t.bump = bump;
        t.trigger_id = trigger_id;
        t.side = side;
        t.kind = kind;
        t.flags = flags;
        t.sub_index = sub_index;
        t._reserved = [0u8; 10];
    }
    Ok(())
}
