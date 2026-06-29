//! place_twap_order — create a v3 TWAP (time-sliced) order account, PDA
//! `[b"twap_v3", market, trader, twap_id]`. Validated against the market's
//! lot/tick rules and the current slot. NO funds move and NO book is touched at
//! placement; a separate (matching) exec path slices it over time.
//!
//! accounts: [trader (signer, payer, w), market (program-owned, r),
//!            twap_order (PDA, w, uninit), system_program]
//! data: [twap_id u8][side u8][flags u8][sub_index u8]
//!       [slice_size u64][total_size u64][limit_price u64]
//!       [slot_interval u64][end_slot u64][acceptable_price u64]   — 52 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TWAP_ORDER_SEED;
use crate::state::{Market, TwapOrderV3, TWAP_ORDER_V3_DISC};
use crate::twap_order::validate_twap_params;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const TWAP_LEN: usize = core::mem::size_of::<TwapOrderV3>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, market, twap_order, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 52 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let twap_id = data[0];
    let side = data[1];
    let flags = data[2];
    let sub_index = data[3];
    let slice_size_lots = u64::from_le_bytes(data[4..12].try_into().unwrap());
    let total_size_lots = u64::from_le_bytes(data[12..20].try_into().unwrap());
    let limit_price_ticks = u64::from_le_bytes(data[20..28].try_into().unwrap());
    let slot_interval = u64::from_le_bytes(data[28..36].try_into().unwrap());
    let end_slot = u64::from_le_bytes(data[36..44].try_into().unwrap());
    let acceptable_price_ticks = u64::from_le_bytes(data[44..52].try_into().unwrap());

    assert_signer(trader)?;
    assert_market(market, program_id)?;

    let (min_base_lots, tick_size) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.min_base_lots, m.tick_size)
    };
    let now = Clock::get()?.slot;
    validate_twap_params(
        side,
        slice_size_lots,
        total_size_lots,
        limit_price_ticks,
        slot_interval,
        acceptable_price_ticks,
        end_slot,
        now,
        min_base_lots,
        tick_size,
    )
    .map_err(|_| ProgramError::InvalidArgument)?;

    assert_uninitialized(twap_order)?;
    let id_arr = [twap_id];
    let bump = assert_pda(
        twap_order,
        &[
            TWAP_ORDER_SEED,
            &market.key()[..],
            &trader.key()[..],
            &id_arr[..],
        ],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(TWAP_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TWAP_ORDER_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&trader.key()[..]),
        Seed::from(&id_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        trader,
        twap_order,
        system_program,
        lamports,
        TWAP_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let t = &mut *(twap_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut TwapOrderV3);
        t.disc = TWAP_ORDER_V3_DISC;
        t.trader = *trader.key();
        t.market = *market.key();
        t.slice_size_lots = slice_size_lots;
        t.total_size_lots = total_size_lots;
        t.size_executed_lots = 0;
        t.limit_price_ticks = limit_price_ticks;
        t.start_slot = now;
        t.slot_interval = slot_interval;
        t.end_slot = end_slot;
        t.last_slice_at_slot = 0;
        t.acceptable_price_ticks = acceptable_price_ticks;
        t.bump = bump;
        t.twap_id = twap_id;
        t.side = side;
        t.flags = flags | crate::state::TWAP_FLAG_ACTIVE;
        t.sub_index = sub_index;
        t._reserved = [0u8; 3];
    }
    Ok(())
}
