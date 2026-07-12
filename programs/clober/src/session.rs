//! Session keys (ephemeral ER signer) — opt-in, additive.
//!
//! A trader can authorize an EPHEMERAL keypair (`session_signer`) to act on their
//! behalf for a bounded window, so a client trading on the ER does not have to
//! sign every order with the cold wallet. The session signer can ONLY do what the
//! session-authorized instruction variants allow (place/cancel) — never withdraw
//! or change collateral. The token is a PDA `[SESSION_SEED, owner, session_signer]`
//! the owner creates and can revoke at any time, and it auto-expires.
//!
//! Design note (safety): this is purely ADDITIVE. The original `*` trade
//! instructions are unchanged; the session variants share the exact same core
//! logic via a single extracted function, differing ONLY in how the trader
//! identity is authenticated (cold-wallet `Signer` vs. a verified session token).

use anchor_lang::prelude::*;

/// PDA seed for a session token.
pub const SESSION_SEED: &[u8] = b"session";

/// Hard cap on a session's lifetime (7 days) — one approval covers a week of
/// one-click trading. Bounds the blast radius of a leaked session key: it can
/// only place/cancel (never withdraw or move collateral), and the owner can
/// revoke it. Expiry and revocation are enforced against the clock and token
/// clone seen by the executing runtime: session trades run on the ER, so both
/// bounds hold to the same degree the ER's clock and account clones are honest
/// — i.e. within the same single-sequencer trust the rest of the ER path already
/// assumes. The custody bound (place/cancel only, own trader_state only) does
/// NOT depend on that trust and holds unconditionally.
pub const MAX_SESSION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A revocable, auto-expiring authorization for `session_signer` to act for
/// `owner`. 105 bytes + 8 disc.
#[account]
pub struct SessionTokenAccount {
    /// The real trader the session acts on behalf of.
    pub owner: Pubkey,
    /// The ephemeral key allowed to sign session-authorized instructions.
    pub session_signer: Pubkey,
    /// Unix seconds after which the session is invalid (checked on every use).
    pub expires_at_unix: i64,
    /// Market scope. `Pubkey::default()` (all-zero) = the
    /// session may act on ANY market (legacy/opt-out behaviour). A specific market
    /// key = the session is restricted to that ONE market, so a leaked session key
    /// cannot dump the owner's collateral across every market. Enforced in `verify`.
    pub scope_market: Pubkey,
    pub bump: u8,
}

impl SessionTokenAccount {
    pub const LEN: usize = 32 + 32 + 8 + 32 + 1;
}

/// Verify a session token authorizes `signer` to act on `market` right now.
/// Fail-closed: wrong signer ⇒ Unauthorized; past expiry ⇒ SessionExpired;
/// out-of-scope market ⇒ Unauthorized.
pub fn verify(
    token: &SessionTokenAccount,
    signer: Pubkey,
    now_unix: i64,
    market: Pubkey,
) -> Result<()> {
    require_keys_eq!(
        token.session_signer,
        signer,
        crate::errors::CloberError::Unauthorized
    );
    require!(
        now_unix <= token.expires_at_unix,
        crate::errors::CloberError::SessionExpired
    );
    // Enforce market scope. Default (all-zero) = unrestricted.
    require!(
        token.scope_market == Pubkey::default() || token.scope_market == market,
        crate::errors::CloberError::Unauthorized
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
            scope_market: Pubkey::default(),
            bump: 0,
        }
    }

    #[test]
    fn valid_unexpired_session_passes() {
        let t = tok([9u8; 32], 1_000);
        let m = Pubkey::new_from_array([7u8; 32]);
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 999, m).is_ok());
        // Exactly at expiry is still valid (<=).
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 1_000, m).is_ok());
    }

    #[test]
    fn wrong_signer_rejected() {
        let t = tok([9u8; 32], 1_000);
        let m = Pubkey::new_from_array([7u8; 32]);
        assert!(verify(&t, Pubkey::new_from_array([8u8; 32]), 999, m).is_err());
    }

    #[test]
    fn expired_session_rejected() {
        let t = tok([9u8; 32], 1_000);
        let m = Pubkey::new_from_array([7u8; 32]);
        assert!(verify(&t, Pubkey::new_from_array([9u8; 32]), 1_001, m).is_err());
    }

    #[test]
    fn out_of_scope_market_rejected() {
        // A market-scoped session rejects a different market but allows
        // its own; a default-scope session allows any market.
        let mut t = tok([9u8; 32], 1_000);
        let m_a = Pubkey::new_from_array([7u8; 32]);
        let m_b = Pubkey::new_from_array([8u8; 32]);
        let signer = Pubkey::new_from_array([9u8; 32]);
        // Default scope ⇒ any market ok.
        assert!(verify(&t, signer, 999, m_a).is_ok());
        assert!(verify(&t, signer, 999, m_b).is_ok());
        // Scoped to m_a ⇒ m_a ok, m_b rejected.
        t.scope_market = m_a;
        assert!(verify(&t, signer, 999, m_a).is_ok());
        assert!(verify(&t, signer, 999, m_b).is_err());
    }

    #[test]
    fn ttl_cap_is_seven_days() {
        assert_eq!(MAX_SESSION_TTL_SECONDS, 604_800);
    }
}
