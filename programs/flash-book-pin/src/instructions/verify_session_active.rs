//! verify_session_active — READ-ONLY gate: succeeds iff a session token is still
//! valid (not revoked, and `now ≤ expires_at_unix`). Reverts `Custom(128)`
//! otherwise. Mutates NO state. This is the check the future session-auth path
//! will run before letting a `session_signer` act for the `owner`.
//!
//! The token is bound to its canonical PDA `[b"session", owner, session_signer]`
//! using its OWN stored owner/signer (the seed components), so only a genuine,
//! program-created session token passes.
//!
//! accounts: [session_token (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::SESSION_SEED;
use crate::state::{SessionToken, SESSION_TOKEN_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [session_token, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_owned_by(session_token, pid)?;
    assert_disc(session_token, &SESSION_TOKEN_DISC)?;

    let now = Clock::get()?.unix_timestamp;
    let (owner, signer, revoked, expires_at) = {
        let d = session_token.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const SessionToken) };
        (t.owner, t.session_signer, t.revoked, t.expires_at_unix)
    };
    // Confirm it sits at its canonical PDA for (owner, session_signer).
    assert_pda(session_token, &[SESSION_SEED, &owner[..], &signer[..]], pid)?;

    if revoked != 0 || now > expires_at {
        return Err(ProgramError::Custom(128));
    }
    Ok(())
}
