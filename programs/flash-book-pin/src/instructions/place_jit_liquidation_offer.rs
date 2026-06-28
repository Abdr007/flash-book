//! place_jit_liquidation_offer — a maker pre-commits an offer to fill a future
//! liquidation at a price tighter than the synthetic limit, recorded in a PDA
//! `[b"jit_liq_offer", market, maker, nonce]`. NO token escrow — the maker's
//! collateral covers the fill when a liquidation matches the offer (the auction,
//! a follow-up). Maker-gated; price must be tick-aligned. Faithful port of the
//! Anchor `place_jit_liquidation_offer`.
//!
//! accounts: [maker (signer, payer, w), market (program-owned, r),
//!            jit_offer (PDA, w, uninit), system_program]
//! data: [nonce u32][target_trader Pubkey(32)][side u8][offer_price_ticks u64]
//!       [max_size_lots u64][expires_at_slot u64][maker_sub_index u8]   (62 bytes)

use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_pda, assert_signer};
use crate::seeds::JIT_LIQ_OFFER_SEED;
use crate::state::{JitLiquidationOffer, Market, JIT_LIQ_OFFER_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const OFFER_LEN: usize = core::mem::size_of::<JitLiquidationOffer>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [maker, market, jit_offer, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 62 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let mut target_trader = [0u8; 32];
    target_trader.copy_from_slice(&data[4..36]);
    let side = data[36];
    let offer_price_ticks = u64::from_le_bytes(data[37..45].try_into().unwrap());
    let max_size_lots = u64::from_le_bytes(data[45..53].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[53..61].try_into().unwrap());
    let maker_sub_index = data[61];
    if side > 1 || offer_price_ticks == 0 || max_size_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(maker)?;
    assert_market(market, program_id)?;
    let tick = {
        let d = market.try_borrow_data()?;
        unsafe { (*(d.as_ptr() as *const Market)).tick_size }
    };
    if tick > 0 && offer_price_ticks % tick != 0 {
        return Err(ProgramError::Custom(2)); // price not on tick
    }
    let now = Clock::get()?.slot;
    if expires_at_slot != 0 && expires_at_slot <= now {
        return Err(ProgramError::InvalidArgument);
    }

    // ── create the offer PDA ────────────────────────────────────────────
    let nonce_bytes = nonce.to_le_bytes();
    let bump = assert_pda(
        jit_offer,
        &[JIT_LIQ_OFFER_SEED, &market.key()[..], &maker.key()[..], &nonce_bytes],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(OFFER_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(JIT_LIQ_OFFER_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&maker.key()[..]),
        Seed::from(&nonce_bytes[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        maker, jit_offer, system_program, lamports, OFFER_LEN as u64, program_id, &signer,
    )?;

    unsafe {
        let o = &mut *(jit_offer.borrow_mut_data_unchecked().as_mut_ptr() as *mut JitLiquidationOffer);
        o.disc = JIT_LIQ_OFFER_DISC;
        o.bump = bump;
        o.side = side;
        o.maker_sub_index = maker_sub_index;
        o._pad0 = [0u8; 1];
        o.nonce = nonce;
        o.market = *market.key();
        o.maker = *maker.key();
        o.target_trader = target_trader;
        o.offer_price_ticks = offer_price_ticks;
        o.max_size_lots = max_size_lots;
        o.remaining_size_lots = max_size_lots;
        o.created_at_slot = now;
        o.expires_at_slot = expires_at_slot;
    }
    Ok(())
}
