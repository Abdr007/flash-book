//! initialize_market — create a market account, PDA
//! `[b"market", base_mint, quote_mint]`, owned by this program.
//!
//! Secure-by-default: the authority signs, the market must be fresh, the PDA is
//! re-derived from the two mints, and the parameters are bounds-checked. The
//! creator (`authority`) is set as the initial `sequencer` (the key authorized
//! to settle fills); a later `set_market_sequencer` instruction (not yet ported)
//! will rotate it.
//!
//! accounts: [authority (signer, payer, w), market (PDA, w),
//!            base_mint (r), quote_mint (r), system_program]
//! data (40 bytes LE):
//!   tick_size: u64, mark_price_ticks: u64, taker_fee_bps: u32,
//!   maker_rebate_bps: i32, min_base_lots: u64, max_oi_base_lots: u64

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::MARKET_SEED;
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const MARKET_LEN: usize = core::mem::size_of::<Market>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, base_mint, quote_mint, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 40 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let tick_size = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let mark_price_ticks = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let taker_fee_bps = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let maker_rebate_bps = i32::from_le_bytes(data[20..24].try_into().unwrap());
    let min_base_lots = u64::from_le_bytes(data[24..32].try_into().unwrap());
    let max_oi_base_lots = u64::from_le_bytes(data[32..40].try_into().unwrap());

    // ── parameter bounds ────────────────────────────────────────────────
    if tick_size == 0 || mark_price_ticks == 0 || min_base_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if taker_fee_bps > crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }
    // maker rebate must not exceed the taker fee (else fills are net-negative).
    if maker_rebate_bps < 0 || (maker_rebate_bps as u32) > taker_fee_bps {
        return Err(ProgramError::InvalidArgument);
    }
    if max_oi_base_lots < min_base_lots {
        return Err(ProgramError::InvalidArgument);
    }
    if base_mint.key() == quote_mint.key() {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(authority)?;
    assert_uninitialized(market)?;
    let bump = assert_pda(
        market,
        &[MARKET_SEED, &base_mint.key()[..], &quote_mint.key()[..]],
        program_id,
    )?;

    // ── create the market PDA (signed by its seeds) ─────────────────────
    let lamports = Rent::get()?.minimum_balance(MARKET_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(&base_mint.key()[..]),
        Seed::from(&quote_mint.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        market,
        system_program,
        lamports,
        MARKET_LEN as u64,
        program_id,
        &signer,
    )?;

    // ── stamp the freshly-created market ────────────────────────────────
    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.disc = MARKET_DISC;
        m.sequencer = *authority.key();
        m.cum_funding_index = [0u8; 16];
        m.long_oi_lots = 0;
        m.short_oi_lots = 0;
        m.tick_size = tick_size;
        m.taker_fee_bps = taker_fee_bps;
        m.maker_rebate_bps = maker_rebate_bps;
        m.mark_price_ticks = mark_price_ticks;
        m.min_base_lots = min_base_lots;
        m.max_oi_base_lots = max_oi_base_lots;
        m.total_fees_collected = 0;
    }
    Ok(())
}
