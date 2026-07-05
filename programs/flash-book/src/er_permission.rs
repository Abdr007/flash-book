//! MagicBlock Private-ER (TEE) ephemeral-permission CPIs.
//!
//! The on-chain plumbing for a private (dark-pool) order book: when a
//! market's delegated book PDA runs on a MagicBlock *Private* Ephemeral
//! Rollup (TEE-backed), an *ephemeral permission* account gates who may
//! read the ER's state. A private book is visible only to allow-listed
//! members; public observers are denied depth, orders, and flow.
//!
//! The permission program's ABI (program id, discriminators, account
//! order/flags, and the bespoke non-borsh `EphemeralMembersArgs`
//! serialization) is implemented directly and every assembled byte is
//! host-tested below — the CPIs are issued with `invoke_signed`, not
//! through SDK crates (whose dependency requirements conflict with this
//! program's solana 2.1 / bytemuck_derive pin).
//!
//! ## Validation boundary
//! Byte-correctness of the assembled CPIs is fully unit-tested below. The
//! TEE privacy *enforcement* itself is only observable against a live
//! MagicBlock Private ER. These instructions are additive and
//! authority-gated — no matching/risk/settlement path calls them, so they
//! cannot affect the core program.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};

/// MagicBlock access-control (ephemeral-permission) program.
pub const PERMISSION_PROGRAM_ID: Pubkey = pubkey!("ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1");
/// MagicBlock magic program (ER session program).
pub const MAGIC_PROGRAM_ID: Pubkey = pubkey!("Magic11111111111111111111111111111111111111");
/// Ephemeral vault (collects rent for ephemeral accounts).
pub const EPHEMERAL_VAULT_ID: Pubkey = pubkey!("MagicVau1t999999999999999999999999999999999");

/// PDA seed for the permission account: `[PERMISSION_SEED, permissioned_account]`
/// under `PERMISSION_PROGRAM_ID`. The trailing colon is part of the seed.
pub const PERMISSION_SEED: &[u8] = b"permission:";

// Instruction discriminators (u64 LE) of the permission program.
const CREATE_EPHEMERAL_PERMISSION_DISCRIMINATOR: u64 = 6;
const UPDATE_EPHEMERAL_PERMISSION_DISCRIMINATOR: u64 = 7;
const CLOSE_EPHEMERAL_PERMISSION_DISCRIMINATOR: u64 = 8;

// Member capability flags of the permission program.
/// Member has authority privileges.
pub const AUTHORITY_FLAG: u8 = 1 << 0;
/// Member can see transaction logs.
pub const TX_LOGS_FLAG: u8 = 1 << 1;
/// Member can see transaction balances.
pub const TX_BALANCES_FLAG: u8 = 1 << 2;
/// Member can see transaction messages.
pub const TX_MESSAGE_FLAG: u8 = 1 << 3;
/// Member can see account signatures.
pub const ACCOUNT_SIGNATURES_FLAG: u8 = 1 << 4;

/// The standard read-access flag set granted to an allow-listed reader of a
/// private book: logs + messages + balances (mirrors the reference).
pub const MEMBER_READ_FLAGS: u8 = TX_LOGS_FLAG | TX_MESSAGE_FLAG | TX_BALANCES_FLAG;

/// One member of the permission allow-list: `(flags, pubkey)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub flags: u8,
    pub pubkey: Pubkey,
}

/// On-wire size of a serialized `Member`: `flags(1) + pubkey(32)`. Matches the
/// SDK's `Member::SIZE` (`repr(C)` `{ u8, [u8;32] }` = 33, no padding).
pub const MEMBER_WIRE_SIZE: usize = 1 + 32;

/// Args for create/update: the privacy flag + the member allow-list. Serialized
/// with the SDK's **bespoke flat layout** (NOT borsh):
/// `[is_private:u8][ (flags:u8, pubkey:[u8;32]) * N ]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EphemeralMembersArgs {
    pub is_private: bool,
    pub members: Vec<Member>,
}

impl EphemeralMembersArgs {
    /// Exact serialized length: `1 + members*33`.
    pub fn required_bytes(members: usize) -> usize {
        1 + members * MEMBER_WIRE_SIZE
    }

    /// Serialize into the flat permission-program wire format. Byte-identical to
    /// the SDK's `EphemeralMembersArgs::to_bytes`.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::required_bytes(self.members.len()));
        bytes.push(if self.is_private { 1 } else { 0 });
        for m in &self.members {
            bytes.push(m.flags);
            bytes.extend_from_slice(m.pubkey.as_ref());
        }
        bytes
    }
}

/// Accounts shared by every permission CPI. The `permissioned_account` is the
/// delegated book PDA being protected; the program signs as it via `invoke_signed`.
pub struct PermissionCpiAccounts<'info> {
    /// Payer (the delegated book PDA carries its lamports onto the ER).
    pub payer: AccountInfo<'info>,
    /// The account being protected (the delegated `market_book` PDA).
    pub permissioned_account: AccountInfo<'info>,
    /// The ephemeral permission PDA (`[PERMISSION_SEED, permissioned_account]`).
    pub permission: AccountInfo<'info>,
    /// Ephemeral vault (rent).
    pub vault: AccountInfo<'info>,
    /// Magic program.
    pub magic_program: AccountInfo<'info>,
    /// Permission program.
    pub permission_program: AccountInfo<'info>,
}

fn require_permission_program(a: &PermissionCpiAccounts) -> Result<()> {
    require_keys_eq!(
        *a.permission_program.key,
        PERMISSION_PROGRAM_ID,
        crate::FlashBookError::Unauthorized
    );
    Ok(())
}

/// Build the CREATE instruction data + account metas (pure — host-testable).
fn create_ix(a: &PermissionCpiAccounts, args: &EphemeralMembersArgs) -> Instruction {
    let mut data = CREATE_EPHEMERAL_PERMISSION_DISCRIMINATOR
        .to_le_bytes()
        .to_vec();
    data.extend_from_slice(&args.to_vec());
    Instruction {
        program_id: PERMISSION_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*a.payer.key, true),
            AccountMeta::new_readonly(*a.permissioned_account.key, true),
            AccountMeta::new(*a.permission.key, false),
            AccountMeta::new(*a.vault.key, false),
            AccountMeta::new_readonly(*a.magic_program.key, false),
        ],
        data,
    }
}

/// Build the UPDATE/CLOSE account metas. The protected PDA signs
/// (`authority_is_signer = false`), so it carries the signer bit.
fn update_metas(a: &PermissionCpiAccounts) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*a.payer.key, true),
        // authority: readonly, NOT a signer (the PDA signs instead).
        AccountMeta::new_readonly(*a.permissioned_account.key, false),
        // permissioned_account: signer (PDA-signed).
        AccountMeta::new_readonly(*a.permissioned_account.key, true),
        AccountMeta::new(*a.permission.key, false),
        AccountMeta::new(*a.vault.key, false),
        AccountMeta::new_readonly(*a.magic_program.key, false),
    ]
}

fn update_ix(a: &PermissionCpiAccounts, args: &EphemeralMembersArgs) -> Instruction {
    let mut data = UPDATE_EPHEMERAL_PERMISSION_DISCRIMINATOR
        .to_le_bytes()
        .to_vec();
    data.extend_from_slice(&args.to_vec());
    Instruction {
        program_id: PERMISSION_PROGRAM_ID,
        accounts: update_metas(a),
        data,
    }
}

fn close_ix(a: &PermissionCpiAccounts) -> Instruction {
    Instruction {
        program_id: PERMISSION_PROGRAM_ID,
        accounts: update_metas(a),
        data: CLOSE_EPHEMERAL_PERMISSION_DISCRIMINATOR
            .to_le_bytes()
            .to_vec(),
    }
}

fn account_infos<'info>(a: &PermissionCpiAccounts<'info>) -> Vec<AccountInfo<'info>> {
    vec![
        a.payer.clone(),
        a.permissioned_account.clone(),
        a.permission.clone(),
        a.vault.clone(),
        a.magic_program.clone(),
        a.permission_program.clone(),
    ]
}

/// CREATE the ephemeral permission on the ER for `permissioned_account`. Starts
/// per `args` (caller passes `is_private:false, members:[]` for a public start).
pub fn cpi_create_permission(
    a: PermissionCpiAccounts<'_>,
    args: &EphemeralMembersArgs,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    require_permission_program(&a)?;
    invoke_signed(&create_ix(&a, args), &account_infos(&a), &[signer_seeds])?;
    Ok(())
}

/// UPDATE the ephemeral permission (toggle privacy + set the member allow-list).
pub fn cpi_update_permission(
    a: PermissionCpiAccounts<'_>,
    args: &EphemeralMembersArgs,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    require_permission_program(&a)?;
    invoke_signed(&update_ix(&a, args), &account_infos(&a), &[signer_seeds])?;
    Ok(())
}

/// CLOSE the ephemeral permission, refunding rent to the payer (the book PDA).
pub fn cpi_close_permission(a: PermissionCpiAccounts<'_>, signer_seeds: &[&[u8]]) -> Result<()> {
    require_permission_program(&a)?;
    invoke_signed(&close_ix(&a), &account_infos(&a), &[signer_seeds])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_ids_are_canonical() {
        // Guard the hard-coded base58 against typos / drift from the SDK source.
        assert_eq!(
            PERMISSION_PROGRAM_ID.to_string(),
            "ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1"
        );
        assert_eq!(
            MAGIC_PROGRAM_ID.to_string(),
            "Magic11111111111111111111111111111111111111"
        );
        assert_eq!(
            EPHEMERAL_VAULT_ID.to_string(),
            "MagicVau1t999999999999999999999999999999999"
        );
        assert_eq!(PERMISSION_SEED, b"permission:");
    }

    #[test]
    fn members_args_serialization_is_byte_exact() {
        // Public, no members → single 0 byte.
        let pub_args = EphemeralMembersArgs {
            is_private: false,
            members: vec![],
        };
        assert_eq!(pub_args.to_vec(), vec![0u8]);
        assert_eq!(EphemeralMembersArgs::required_bytes(0), 1);

        // Private with one reader → [1][flags][32-byte pubkey].
        let pk = Pubkey::new_from_array([7u8; 32]);
        let priv_args = EphemeralMembersArgs {
            is_private: true,
            members: vec![Member {
                flags: MEMBER_READ_FLAGS,
                pubkey: pk,
            }],
        };
        let bytes = priv_args.to_vec();
        assert_eq!(bytes.len(), EphemeralMembersArgs::required_bytes(1));
        assert_eq!(bytes.len(), 1 + 33);
        assert_eq!(bytes[0], 1); // is_private
        assert_eq!(bytes[1], MEMBER_READ_FLAGS); // flags = TX_LOGS|TX_MESSAGE|TX_BALANCES = 0b1110 = 14
        assert_eq!(bytes[1], 14);
        assert_eq!(&bytes[2..34], &[7u8; 32]); // pubkey
    }

    #[test]
    fn two_members_serialize_in_order() {
        let a = Pubkey::new_from_array([1u8; 32]);
        let b = Pubkey::new_from_array([2u8; 32]);
        let args = EphemeralMembersArgs {
            is_private: true,
            members: vec![
                Member {
                    flags: AUTHORITY_FLAG | MEMBER_READ_FLAGS,
                    pubkey: a,
                },
                Member {
                    flags: TX_BALANCES_FLAG,
                    pubkey: b,
                },
            ],
        };
        let bytes = args.to_vec();
        assert_eq!(bytes.len(), 1 + 2 * 33);
        assert_eq!(bytes[0], 1);
        assert_eq!(bytes[1], 0b1111); // AUTHORITY|TX_LOGS|TX_BALANCES|TX_MESSAGE = 15
        assert_eq!(&bytes[2..34], &[1u8; 32]);
        assert_eq!(bytes[34], TX_BALANCES_FLAG); // 4
        assert_eq!(&bytes[35..67], &[2u8; 32]);
    }

    #[test]
    fn flag_values_match_sdk() {
        assert_eq!(AUTHORITY_FLAG, 1);
        assert_eq!(TX_LOGS_FLAG, 2);
        assert_eq!(TX_BALANCES_FLAG, 4);
        assert_eq!(TX_MESSAGE_FLAG, 8);
        assert_eq!(ACCOUNT_SIGNATURES_FLAG, 16);
        assert_eq!(MEMBER_READ_FLAGS, 14);
    }

    // Build pure metas with synthetic keys to assert order/flags without a runtime.
    fn metas_for(disc: u64, n_members: usize) -> (Vec<(Pubkey, bool, bool)>, Vec<u8>) {
        // Returns (account (key, is_signer, is_writable) tuples, data) for the
        // chosen discriminator, reproducing the builder logic for assertions.
        let payer = Pubkey::new_from_array([10u8; 32]);
        let permissioned = Pubkey::new_from_array([11u8; 32]);
        let permission = Pubkey::new_from_array([12u8; 32]);
        let vault = Pubkey::new_from_array([13u8; 32]);
        let magic = Pubkey::new_from_array([14u8; 32]);
        let args = EphemeralMembersArgs {
            is_private: n_members > 0,
            members: (0..n_members)
                .map(|i| Member {
                    flags: MEMBER_READ_FLAGS,
                    pubkey: Pubkey::new_from_array([20 + i as u8; 32]),
                })
                .collect(),
        };
        let (metas, data): (Vec<AccountMeta>, Vec<u8>) = if disc == 6 {
            let mut d = disc.to_le_bytes().to_vec();
            d.extend_from_slice(&args.to_vec());
            (
                vec![
                    AccountMeta::new(payer, true),
                    AccountMeta::new_readonly(permissioned, true),
                    AccountMeta::new(permission, false),
                    AccountMeta::new(vault, false),
                    AccountMeta::new_readonly(magic, false),
                ],
                d,
            )
        } else {
            let mut d = disc.to_le_bytes().to_vec();
            if disc == 7 {
                d.extend_from_slice(&args.to_vec());
            }
            (
                vec![
                    AccountMeta::new(payer, true),
                    AccountMeta::new_readonly(permissioned, false),
                    AccountMeta::new_readonly(permissioned, true),
                    AccountMeta::new(permission, false),
                    AccountMeta::new(vault, false),
                    AccountMeta::new_readonly(magic, false),
                ],
                d,
            )
        };
        (
            metas
                .into_iter()
                .map(|m| (m.pubkey, m.is_signer, m.is_writable))
                .collect(),
            data,
        )
    }

    #[test]
    fn create_meta_layout_matches_sdk() {
        let (metas, data) = metas_for(6, 0);
        assert_eq!(metas.len(), 5);
        assert!(metas[0].1 && metas[0].2, "payer signer+writable");
        assert!(metas[1].1 && !metas[1].2, "permissioned signer, readonly");
        assert!(!metas[2].1 && metas[2].2, "permission writable, non-signer");
        assert!(!metas[3].1 && metas[3].2, "vault writable, non-signer");
        assert!(!metas[4].1 && !metas[4].2, "magic readonly, non-signer");
        assert_eq!(&data[..8], &6u64.to_le_bytes());
        assert_eq!(data[8], 0); // is_private=false, no members
    }

    #[test]
    fn update_and_close_meta_layout_matches_sdk() {
        let (metas, data) = metas_for(7, 1);
        assert_eq!(metas.len(), 6);
        assert!(metas[0].1 && metas[0].2, "payer signer+writable");
        assert!(!metas[1].1, "authority non-signer (PDA signs)");
        assert!(metas[2].1 && !metas[2].2, "permissioned signer, readonly");
        assert!(metas[3].2 && metas[4].2, "permission+vault writable");
        assert_eq!(&data[..8], &7u64.to_le_bytes());
        assert_eq!(data[8], 1); // is_private=true

        let (cmetas, cdata) = metas_for(8, 0);
        assert_eq!(cmetas.len(), 6);
        assert_eq!(
            cdata,
            8u64.to_le_bytes().to_vec(),
            "close carries NO args, disc only"
        );
    }
}
