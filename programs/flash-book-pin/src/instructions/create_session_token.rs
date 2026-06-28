//! create_session_token — the owner authorizes an ephemeral `session_signer` to
//! act on their behalf until `now + ttl`, PDA `[b"session", owner,
//! session_signer]`. Owner-signed; TTL bounded to `MAX_SESSION_TTL_SECONDS` (24h)
//! so a session can never be long-lived. NO funds, NO book. The session-auth
//! check on trade paths is a later batch.
//!
//! accounts: [owner (signer, payer, w), session_signer (any — key only),
//!            session_token (PDA, w, uninit), system_program]
//! data: ttl_seconds (i64 LE)

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::SESSION_SEED;
use crate::state::{SessionToken, MAX_SESSION_TTL_SECONDS, SESSION_TOKEN_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const SESSION_LEN: usize = core::mem::size_of::<SessionToken>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [owner, session_signer, session_token, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let ttl_seconds = i64::from_le_bytes(data[0..8].try_into().unwrap());
    if ttl_seconds <= 0 || ttl_seconds > MAX_SESSION_TTL_SECONDS {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(owner)?;
    let now = Clock::get()?.unix_timestamp;
    let expires_at_unix = now
        .checked_add(ttl_seconds)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    assert_uninitialized(session_token)?;
    let bump = assert_pda(
        session_token,
        &[SESSION_SEED, &owner.key()[..], &session_signer.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(SESSION_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(SESSION_SEED),
        Seed::from(&owner.key()[..]),
        Seed::from(&session_signer.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        owner,
        session_token,
        system_program,
        lamports,
        SESSION_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let t = &mut *(session_token.borrow_mut_data_unchecked().as_mut_ptr() as *mut SessionToken);
        t.disc = SESSION_TOKEN_DISC;
        t.owner = *owner.key();
        t.session_signer = *session_signer.key();
        t.expires_at_unix = expires_at_unix;
        t.bump = bump;
        t.revoked = 0;
        t._pad = [0u8; 6];
    }
    Ok(())
}
