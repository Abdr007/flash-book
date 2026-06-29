//! init_fill_commitment — one-time per market: create the FillCommitmentAccount
//! ring (`[b"fill_commit", market]`). Authority-gated. Port of the Anchor
//! `init_fill_commitment`.
//!
//! HONEST-ENFORCEMENT NOTE (audit 2026-06): the Anchor original ALSO flips the
//! sticky `Market.fill_commitment_required = 1` here, because Anchor's matcher
//! (`place_taker_order_v2`) `buffer_push`es a keccak commit per crossed fill and
//! `apply_fill` `buffer_settle`s it under that flag. The Pinocchio port does NOT
//! YET wire the producer/consumer (the matcher pushes nothing; `apply_fill`
//! settles nothing) — and pin has no keccak in the SBF runtime to recompute the
//! preimage. Arming the flag would advertise a settlement-authenticity guarantee
//! the program does not enforce. So this port allocates+initializes the ring but
//! LEAVES THE FLAG UNSET until the matcher push + `apply_fill` settle (with a
//! byte-exact producer==consumer preimage e2e test, mirroring Anchor's) land.
//! See AUDIT_SCOPE residuals. The ring primitives (`fill_commitment.rs`) are
//! Kani-proven and ready; only the two hot-path call sites + keccak remain.
//!
//! accounts: [authority (signer, payer), market (program-owned, w),
//!            fill_commitment (PDA, w, uninit), system_program]
//! data: (none — capacity is the default FILL_RING_CAP)

use crate::cpi::create_pda_account;
use crate::fill_commitment::{
    buffer_init, fill_commit_account_len, FILL_COMMIT_SEED, FILL_RING_CAP,
};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer, assert_uninitialized};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

/// Offset of `Market.fill_commitment_required` (after `book_delegated_at_slot`).
const MKT_FILL_COMMIT_REQUIRED: usize = 240;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, fill_commitment, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // Create the ring PDA.
    assert_uninitialized(fill_commitment)?;
    let bump = assert_pda(fill_commitment, &[FILL_COMMIT_SEED, &market.key()[..]], pid)?;
    let space = fill_commit_account_len(FILL_RING_CAP as usize);
    let lamports = Rent::get()?.minimum_balance(space);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(FILL_COMMIT_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(authority, fill_commitment, system_program, lamports, space as u64, pid, &signer)?;

    // Stamp the ring header.
    {
        let mut d = fill_commitment.try_borrow_mut_data()?;
        buffer_init(&mut d, market.key(), FILL_RING_CAP, bump).map_err(|_| ProgramError::InvalidAccountData)?;
    }

    // DO NOT arm the sticky flag yet: no settlement path consumes the ring in
    // this port, and pin has no in-SBF keccak to recompute the commit, so arming
    // it would advertise an unenforced guarantee (see the module note). Leave
    // `fill_commitment_required = 0`. Defensive: ensure it is explicitly cleared
    // (the account is freshly zero-initialized, but make the intent loud).
    unsafe {
        let m = market.borrow_mut_data_unchecked();
        m[MKT_FILL_COMMIT_REQUIRED] = 0;
    }
    Ok(())
}
