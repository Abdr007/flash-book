//! MagicBlock Ephemeral Rollups delegation CPI (in-house implementation).
//!
//! Why this exists in-house instead of using the upstream
//! `ephemeral-rollups-sdk` crate:
//!
//!   - `ephemeral-rollups-sdk` 0.10–0.13 contain code paths that depend
//!     on `Pubkey::as_array()` — a method introduced in solana-program
//!     2.2.x. Our stack is pinned to solana 2.1.x by spl-token v6 (which
//!     can't bump until upstream SPL releases a 2.2-compatible v7).
//!   - `magicblock-delegation-program-api` ≥0.1.1 declares
//!     `solana-instruction ^3.0.0` / `solana-pubkey ^3.0.0` — also out
//!     of reach.
//!
//! The MagicBlock delegation program's ABI is small and stable (program
//! ID + 1-byte discriminator + Borsh args). Implementing the CPI here
//! by hand is ~100 lines of code, version-agnostic, and easier to audit
//! than pulling a dependency we can barely build against. When the
//! upstream SDK lands a 2.1-compatible release we can swap to it
//! drop-in via the `Delegate`/`Undelegate` ix builders below.
//!
//! ABI references (verified against the v0.3.0 magicblock-delegation-
//! program-api source):
//!
//!   - Program ID: DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh
//!   - Delegate discriminator: 0u8
//!   - Undelegate discriminator: 3u8
//!   - PDA seeds:
//!       buffer       = [b"buffer", delegated_account]            (under owner_program)
//!       record       = [b"delegation", delegated_account]        (under delegation program)
//!       metadata     = [b"delegation-metadata", delegated_account] (under delegation program)
//!
//! Account layouts mirror the upstream `cpi::delegate` builder.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    pubkey,
};

/// MagicBlock Delegation Program ID. Mainnet + devnet share this address.
/// Confirmed against `magicblock-delegation-program-api 0.3.0` source.
pub const DELEGATION_PROGRAM_ID: Pubkey =
    pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

/// PDA seed prefix for the delegate buffer (lives under `owner_program`).
pub const DELEGATE_BUFFER_TAG: &[u8] = b"buffer";
/// PDA seed prefix for the delegation record (lives under DELEGATION_PROGRAM_ID).
pub const DELEGATION_RECORD_TAG: &[u8] = b"delegation";
/// PDA seed prefix for the delegation metadata (lives under DELEGATION_PROGRAM_ID).
pub const DELEGATION_METADATA_TAG: &[u8] = b"delegation-metadata";

/// Discriminator for the `Delegate` instruction.
const DELEGATE_DISCRIMINATOR: u8 = 0;
/// Discriminator for the `Undelegate` instruction.
const UNDELEGATE_DISCRIMINATOR: u8 = 3;

/// Borsh-serialized argument struct for the Delegate ix. Layout matches
/// `magicblock-delegation-program-api::args::DelegateArgs` byte-for-byte.
#[derive(Default, Debug, Clone, AnchorSerialize, AnchorDeserialize)]
pub struct DelegateArgs {
    /// Frequency at which the validator commits the account state if the
    /// owning program doesn't trigger commits explicitly.
    pub commit_frequency_ms: u32,
    /// Seeds used to re-derive the PDA inside the delegation program.
    /// Must INCLUDE the bump as the final element if the PDA is
    /// canonical-bump.
    pub seeds: Vec<Vec<u8>>,
    /// Optional validator authority. Pass None for permissionless
    /// validator selection.
    pub validator: Option<Pubkey>,
}

/// Derive the buffer PDA for a delegated account. The buffer is owned by
/// the OWNER PROGRAM (the program that owns the delegated account), not
/// the delegation program. This is intentional — it gives the owner
/// program control over rent and lifetime.
pub fn delegate_buffer_pda(delegated: &Pubkey, owner_program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[DELEGATE_BUFFER_TAG, delegated.as_ref()], owner_program)
}

/// Derive the delegation record PDA. Lives under the delegation program.
pub fn delegation_record_pda(delegated: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[DELEGATION_RECORD_TAG, delegated.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
}

/// Derive the delegation metadata PDA. Lives under the delegation program.
pub fn delegation_metadata_pda(delegated: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[DELEGATION_METADATA_TAG, delegated.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
}

/// Account list for the Delegate CPI.
pub struct DelegateAccounts<'info> {
    pub payer: AccountInfo<'info>,
    pub delegated_account: AccountInfo<'info>,
    pub owner_program: AccountInfo<'info>,
    pub delegate_buffer: AccountInfo<'info>,
    pub delegation_record: AccountInfo<'info>,
    pub delegation_metadata: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
    pub delegation_program: AccountInfo<'info>,
}

/// Build + invoke the Delegate instruction. The delegated PDA is signed
/// via `delegated_seeds` (must include the canonical bump byte).
///
/// SECURITY: the caller MUST verify `delegated_account.owner` is this
/// program before calling — otherwise an attacker could pass a foreign
/// PDA and trick this program into delegating someone else's account.
/// The `delegation_program.key == DELEGATION_PROGRAM_ID` check below is
/// belt-and-suspenders against the wrong program account being passed in.
pub fn cpi_delegate(
    accounts: DelegateAccounts<'_>,
    args: DelegateArgs,
    delegated_seeds: &[&[u8]],
) -> Result<()> {
    require_keys_eq!(
        *accounts.delegation_program.key,
        DELEGATION_PROGRAM_ID,
        crate::FlashBookError::Unauthorized
    );

    let mut data = Vec::with_capacity(64);
    data.push(DELEGATE_DISCRIMINATOR);
    args.serialize(&mut data)?;

    let ix = Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*accounts.payer.key, true),
            // delegated_account is signer-by-PDA via invoke_signed.
            AccountMeta::new(*accounts.delegated_account.key, true),
            AccountMeta::new_readonly(*accounts.owner_program.key, false),
            AccountMeta::new(*accounts.delegate_buffer.key, false),
            AccountMeta::new(*accounts.delegation_record.key, false),
            AccountMeta::new(*accounts.delegation_metadata.key, false),
            AccountMeta::new_readonly(*accounts.system_program.key, false),
        ],
        data,
    };

    invoke_signed(
        &ix,
        &[
            accounts.payer,
            accounts.delegated_account,
            accounts.owner_program,
            accounts.delegate_buffer,
            accounts.delegation_record,
            accounts.delegation_metadata,
            accounts.system_program,
            accounts.delegation_program,
        ],
        &[delegated_seeds],
    )?;
    Ok(())
}

/// Account list for the Undelegate CPI.
pub struct UndelegateAccounts<'info> {
    pub payer: AccountInfo<'info>,
    pub delegated_account: AccountInfo<'info>,
    pub owner_program: AccountInfo<'info>,
    pub buffer: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
    pub delegation_program: AccountInfo<'info>,
}

/// Build + invoke the Undelegate instruction. Returns control of the PDA
/// from the ER back to the owner program.
pub fn cpi_undelegate(
    accounts: UndelegateAccounts<'_>,
    delegated_seeds: &[&[u8]],
) -> Result<()> {
    require_keys_eq!(
        *accounts.delegation_program.key,
        DELEGATION_PROGRAM_ID,
        crate::FlashBookError::Unauthorized
    );

    let data = vec![UNDELEGATE_DISCRIMINATOR];

    let ix = Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*accounts.payer.key, true),
            AccountMeta::new(*accounts.delegated_account.key, true),
            AccountMeta::new_readonly(*accounts.owner_program.key, false),
            AccountMeta::new(*accounts.buffer.key, false),
            AccountMeta::new_readonly(*accounts.system_program.key, false),
        ],
        data,
    };

    invoke_signed(
        &ix,
        &[
            accounts.payer,
            accounts.delegated_account,
            accounts.owner_program,
            accounts.buffer,
            accounts.system_program,
            accounts.delegation_program,
        ],
        &[delegated_seeds],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_program_id_is_canonical() {
        assert_eq!(
            DELEGATION_PROGRAM_ID.to_string(),
            "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh"
        );
    }

    #[test]
    fn delegate_args_borsh_roundtrip() {
        let args = DelegateArgs {
            commit_frequency_ms: 1000,
            seeds: vec![b"market".to_vec(), vec![1, 2, 3]],
            validator: Some(Pubkey::new_unique()),
        };
        let mut buf = Vec::new();
        args.serialize(&mut buf).unwrap();
        let decoded: DelegateArgs = AnchorDeserialize::deserialize(&mut &buf[..]).unwrap();
        assert_eq!(decoded.commit_frequency_ms, args.commit_frequency_ms);
        assert_eq!(decoded.seeds, args.seeds);
        assert_eq!(decoded.validator, args.validator);
    }

    #[test]
    fn delegate_args_with_no_validator_serializes_compact() {
        let args = DelegateArgs {
            commit_frequency_ms: 5000,
            seeds: vec![b"x".to_vec()],
            validator: None,
        };
        let mut buf = Vec::new();
        args.serialize(&mut buf).unwrap();
        // 4 (commit_frequency_ms) + 4 (seeds count) + 4 (seed[0] len) + 1 (seed[0]) + 1 (Option None tag)
        assert_eq!(buf.len(), 14);
        // Last byte is the Option discriminator: 0 for None.
        assert_eq!(*buf.last().unwrap(), 0);
    }

    #[test]
    fn pda_derivations_are_deterministic() {
        let delegated = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let (buf1, _) = delegate_buffer_pda(&delegated, &owner);
        let (buf2, _) = delegate_buffer_pda(&delegated, &owner);
        assert_eq!(buf1, buf2);
        let (rec1, _) = delegation_record_pda(&delegated);
        let (rec2, _) = delegation_record_pda(&delegated);
        assert_eq!(rec1, rec2);
        // Different delegated → different PDAs.
        let other = Pubkey::new_unique();
        let (buf_other, _) = delegate_buffer_pda(&other, &owner);
        assert_ne!(buf1, buf_other);
    }
}
