//! Security guards for the ported instructions (Phase 0).
//!
//! The 8 hot-path handlers ported first deliberately skipped account validation
//! — they pointer-cast their inputs and assumed "an Anchor wrapper validated
//! these upstream". For a STANDALONE program that assumption does NOT hold: a
//! caller can pass any account in any slot. Every lifecycle / collateral
//! instruction added from here MUST gate its inputs through these helpers
//! before pointer-casting or moving funds:
//!
//!   * `assert_signer`        — the account authorized this action.
//!   * `assert_owned_by`      — the account is owned by THIS program (its data
//!                              is a struct we wrote, not attacker bytes).
//!   * `assert_pda`           — the account is the canonical PDA for `seeds`
//!                              (re-derived on-chain), returning its bump.
//!   * `assert_uninitialized` — a to-be-created account is fresh.
//!   * [`check_disc`]         — an existing account carries the expected disc.
//!
//! Together these close the "substitute a fake account" attack an un-guarded
//! pointer-cast is wide open to.

/// Require the first 8 bytes of `data` to equal `expected`. Pure + host-tested.
#[inline]
pub fn check_disc(data: &[u8], expected: &[u8; 8]) -> bool {
    data.len() >= 8 && &data[..8] == expected.as_slice()
}

#[cfg(target_os = "solana")]
mod sol {
    use super::check_disc;
    use crate::book::MARKET_BOOK_SEED;
    use crate::state::MARKET_DISC;
    use pinocchio::{
        account_info::AccountInfo,
        program_error::ProgramError,
        pubkey::{find_program_address, Pubkey},
        ProgramResult,
    };

    /// Require `ai` to have signed the transaction.
    #[inline]
    pub fn assert_signer(ai: &AccountInfo) -> ProgramResult {
        if !ai.is_signer() {
            return Err(ProgramError::MissingRequiredSignature);
        }
        Ok(())
    }

    /// Require `ai` to be owned by `program_id` — i.e. its bytes are a struct
    /// this program created. Without this an attacker passes an account they
    /// control and the handler pointer-casts attacker bytes as trusted state.
    #[inline]
    pub fn assert_owned_by(ai: &AccountInfo, program_id: &Pubkey) -> ProgramResult {
        if !ai.is_owned_by(program_id) {
            return Err(ProgramError::IllegalOwner);
        }
        Ok(())
    }

    /// Re-derive the PDA for `seeds` under `program_id` and require it to equal
    /// `ai.key()`. Returns the canonical bump (needed to sign CPIs as the PDA).
    /// The single most important check the hot-path handlers omit: it binds an
    /// account to its derivation so a caller cannot substitute another account.
    #[inline]
    pub fn assert_pda(
        ai: &AccountInfo,
        seeds: &[&[u8]],
        program_id: &Pubkey,
    ) -> Result<u8, ProgramError> {
        let (expected, bump) = find_program_address(seeds, program_id);
        if &expected != ai.key() {
            return Err(ProgramError::InvalidSeeds);
        }
        Ok(bump)
    }

    /// Require an account to be UNINITIALIZED (zero-length) before creating it —
    /// guards against re-initialization.
    #[inline]
    pub fn assert_uninitialized(ai: &AccountInfo) -> ProgramResult {
        if ai.data_len() != 0 {
            return Err(ProgramError::AccountAlreadyInitialized);
        }
        Ok(())
    }

    /// Require an existing program-owned account to carry `expected` disc.
    #[inline]
    pub fn assert_disc(ai: &AccountInfo, expected: &[u8; 8]) -> ProgramResult {
        let data = ai.try_borrow_data()?;
        if !check_disc(&data, expected) {
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(())
    }

    /// Require `ai` to be a program-owned market (owner + `MARKET_DISC`).
    #[inline]
    pub fn assert_market(ai: &AccountInfo, program_id: &Pubkey) -> ProgramResult {
        assert_owned_by(ai, program_id)?;
        assert_disc(ai, &MARKET_DISC)
    }

    /// Require `book` to be the canonical market_book PDA for `market`
    /// (program-owned + `[b"market_book", market]`). Binds the orderbook to its
    /// market so a caller can't pair market A with book B.
    #[inline]
    pub fn assert_market_book(
        book: &AccountInfo,
        market: &AccountInfo,
        program_id: &Pubkey,
    ) -> ProgramResult {
        assert_owned_by(book, program_id)?;
        assert_pda(book, &[MARKET_BOOK_SEED, &market.key()[..]], program_id)?;
        Ok(())
    }
}

#[cfg(target_os = "solana")]
pub use sol::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_disc_accepts_match_rejects_mismatch() {
        let disc = [0xF1, 0x05, 0xB0, 0x0C, 0x50, 0x53, 0x00, 0x02];
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&disc);
        assert!(check_disc(&buf, &disc));
        let mut wrong = disc;
        wrong[0] ^= 0xFF;
        assert!(!check_disc(&buf, &wrong));
        assert!(!check_disc(&[0u8; 4], &disc)); // too short
    }
}
