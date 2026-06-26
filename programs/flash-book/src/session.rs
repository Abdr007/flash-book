//! Session keys (ephemeral ER signer) — opt-in, additive.
//!
//! A trader can authorize an EPHEMERAL keypair (`session_signer`) to act on their
//! behalf for a bounded window, so a client trading on the ER does not have to
//! sign every order with the cold wallet. The session signer can ONLY do what the
//! session-authorized instruction variants allow (place/cancel) — never withdraw
//! or change collateral. The token is a PDA `[SESSION_SEED, owner, session_signer]`
//! the owner creates and can revoke at any time, and it auto-expires.
//!
//! Design note (safety): this is purely ADDITIVE. The original `*_v2` trade
//! instructions are unchanged; the session variants share the exact same core
//! logic via a single extracted function, differing ONLY in how the trader
//! identity is authenticated (cold-wallet `Signer` vs. a verified session token).

use anchor_lang::prelude::*;

/// PDA seed for a session token.
pub const SESSION_SEED: &[u8] = b"session";

/// Hard cap on a session's lifetime (24h). Bounds the blast radius of a leaked
/// session key — it expires regardless, and the owner can revoke sooner.
pub const MAX_SESSION_TTL_SECONDS: i64 = 24 * 60 * 60;

/// A revocable, auto-expiring authorization for `session_signer` to act for
/// `owner`. 73 bytes + 8 disc.
#[account]
pub struct SessionTokenAccount {
    /// The real trader the session acts on behalf of.
    pub owner: Pubkey,
    /// The ephemeral key allowed to sign session-authorized instructions.
    pub session_signer: Pubkey,
    /// Unix seconds after which the session is invalid (checked on every use).
    pub expires_at_unix: i64,
    pub bump: u8,
}

impl SessionTokenAccount {
    pub const LEN: usize = 32 + 32 + 8 + 1;
}

/// Verify a session token authorizes `signer` to act right now. Fail-closed:
/// wrong signer ⇒ Unauthorized; past expiry ⇒ SessionExpired.
pub fn verify(
    token: &SessionTokenAccount,
    signer: Pubkey,
    now_unix: i64,
) -> Result<()> {
    require_keys_eq!(
        token.session_signer,
        signer,
        crate::errors::FlashBookError::Unauthorized
    );
    require!(
        now_unix <= token.expires_at_unix,
        crate::errors::FlashBookError::SessionExpired
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(signer: [u8; 32], expires: i64) -> SessionTokenAccount {
        SessionTokenAccount {
            owner: Pubkey::new_from_array([1u8; 32]),
            session_signer: Pubkey::new_from_array(signer),
            expires_at_unix: expires,
            bump: 0,
        }
    }

    #[test]
    fn valid_unexpired_session_passes() {
        let t = tok([9u8; 32], 1_000);
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 999).is_ok());
        // Exactly at expiry is still valid (<=).
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 1_000).is_ok());
    }

    #[test]
    fn wrong_signer_rejected() {
        let t = tok([9u8; 32], 1_000);
        assert!(verify(&t, Pubkey::new_from_array([8u8; 32]), 999).is_err());
    }

    #[test]
    fn expired_session_rejected() {
        let t = tok([9u8; 32], 1_000);
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 1_001).is_err());
    }

    #[test]
    fn ttl_cap_is_one_day() {
        assert_eq!(MAX_SESSION_TTL_SECONDS, 86_400);
    }
}
