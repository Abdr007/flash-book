//! open_trader_state — create the per-trader collateral account.
//!
//! PDA `[b"trader_state", trader]`, owned by THIS program. Secure-by-default
//! (unlike the hot-path handlers): the trader must sign, the target account must
//! be fresh, and the PDA is re-derived on-chain before creation so a caller
//! cannot create state at an address they don't control. The account is then
//! stamped with its discriminator + owner; every later handler that touches a
//! TraderState verifies `assert_owned_by` + the discriminator.
//!
//! accounts: [trader (signer, payer, w), trader_state (PDA, w), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TRADER_STATE_SEED;
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const TRADER_STATE_LEN: usize = core::mem::size_of::<TraderState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, trader_state, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(trader)?;
    assert_uninitialized(trader_state)?;
    let bump = assert_pda(
        trader_state,
        &[TRADER_STATE_SEED, &trader.key()[..]],
        program_id,
    )?;

    // ── create the PDA, signed by its own seeds ─────────────────────────
    let lamports = Rent::get()?.minimum_balance(TRADER_STATE_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TRADER_STATE_SEED),
        Seed::from(&trader.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        trader,
        trader_state,
        system_program,
        lamports,
        TRADER_STATE_LEN as u64,
        program_id,
        &signer,
    )?;

    // ── stamp the freshly-created account ───────────────────────────────
    // Safe: we just created `trader_state` with exactly TRADER_STATE_LEN bytes
    // owned by this program; the cast target is `#[repr(C)]` and 8-aligned.
    unsafe {
        let ts =
            &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.disc = TRADER_STATE_DISC;
        ts.trader = *trader.key();
        ts.collateral_quote_lots = 0;
    }
    Ok(())
}
