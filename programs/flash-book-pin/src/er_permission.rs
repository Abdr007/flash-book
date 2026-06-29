//! Hand-rolled MagicBlock ephemeral-permission (access-control) CPIs — pin port.
//! Faithful transcription of the Anchor `er_permission.rs`: same program ids,
//! instruction discriminators, account order/flags, and the SDK's bespoke flat
//! `EphemeralMembersArgs` wire format. No `ephemeral-rollups-sdk` (un-addable
//! here); built on pinocchio CPIs, no_std + no-alloc (fixed stack buffers).
//!
//! Drives a PRIVATE / dark-pool book: a delegated `market_book` PDA gets a
//! permission account on the ER whose member allow-list gates who may read the
//! book's state through the TEE. The book PDA signs each CPI.

use pinocchio::{
    account_info::AccountInfo,
    cpi::slice_invoke_signed,
    instruction::{AccountMeta, Instruction, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

/// MagicBlock access-control program (`ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1`).
pub const PERMISSION_PROGRAM_ID: [u8; 32] = [
    136, 161, 10, 196, 33, 152, 1, 214, 246, 106, 29, 60, 6, 152, 192, 102, 169, 175, 212, 217,
    180, 252, 231, 71, 151, 141, 209, 5, 168, 212, 103, 82,
];
/// MagicBlock magic program (`Magic111…`).
pub const MAGIC_PROGRAM_ID: [u8; 32] = [
    5, 69, 180, 36, 176, 218, 112, 149, 236, 185, 214, 222, 195, 119, 215, 40, 145, 182, 231, 142,
    146, 234, 18, 214, 223, 187, 58, 64, 0, 0, 0, 0,
];
/// Ephemeral vault (`MagicVau1t…`, collects rent).
pub const EPHEMERAL_VAULT_ID: [u8; 32] = [
    5, 69, 180, 36, 224, 197, 24, 97, 240, 41, 76, 112, 66, 34, 84, 78, 202, 127, 133, 79, 194,
    135, 136, 166, 123, 118, 113, 80, 62, 224, 143, 184,
];

/// PDA seed for the permission account: `[PERMISSION_SEED, permissioned_account]`
/// under `PERMISSION_PROGRAM_ID` (note the trailing colon — matches the SDK).
pub const PERMISSION_SEED: &[u8] = b"permission:";

/// Instruction discriminators (u64 LE) of the permission program — verbatim.
pub const CREATE_DISCRIMINATOR: u64 = 6;
pub const UPDATE_DISCRIMINATOR: u64 = 7;
pub const CLOSE_DISCRIMINATOR: u64 = 8;

// Member capability flags (verbatim from the SDK).
pub const AUTHORITY_FLAG: u8 = 1 << 0;
pub const TX_LOGS_FLAG: u8 = 1 << 1;
pub const TX_BALANCES_FLAG: u8 = 1 << 2;
pub const TX_MESSAGE_FLAG: u8 = 1 << 3;
pub const ACCOUNT_SIGNATURES_FLAG: u8 = 1 << 4;

/// Standard read-access set for an allow-listed reader: logs + messages +
/// balances = `0b1110` = 14.
pub const MEMBER_READ_FLAGS: u8 = TX_LOGS_FLAG | TX_MESSAGE_FLAG | TX_BALANCES_FLAG;

/// Max readers on a private book's allow-list.
pub const MAX_PRIVACY_MEMBERS: usize = 32;

const MEMBER_WIRE_SIZE: usize = 1 + 32;
/// Max CPI data: 8-byte disc + 1 is_private byte + MAX members × 33.
pub const MAX_DATA: usize = 8 + 1 + MAX_PRIVACY_MEMBERS * MEMBER_WIRE_SIZE;

/// One member of the permission allow-list: `(flags, pubkey)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub flags: u8,
    pub pubkey: Pubkey,
}

/// Exact serialized length of the args body: `1 + members * 33`.
pub fn members_args_len(members: usize) -> usize {
    1 + members * MEMBER_WIRE_SIZE
}

/// Write `disc(u64 LE) ++ [is_private:u8][ (flags:u8, pubkey:[u8;32]) * N ]` into
/// `buf`, returning the byte length. Byte-identical to the SDK wire format.
/// Caller guarantees `buf.len() >= 8 + members_args_len(members.len())`.
///
/// NOTE: the big stack buffer this writes into lives in the INSTRUCTION frame
/// (not the CPI frame), so the CPI helpers below take a pre-built `&[u8]` —
/// splitting the 1KB-class buffer away from the syscall machinery keeps both
/// frames under the SBF 4KB stack limit.
pub fn write_ix_data(buf: &mut [u8], disc: u64, is_private: bool, members: &[Member]) -> usize {
    buf[0..8].copy_from_slice(&disc.to_le_bytes());
    buf[8] = if is_private { 1 } else { 0 };
    let mut o = 9;
    for m in members {
        buf[o] = m.flags;
        buf[o + 1..o + 33].copy_from_slice(&m.pubkey);
        o += MEMBER_WIRE_SIZE;
    }
    o
}

/// Accounts shared by every permission CPI. `permissioned_account` is the
/// delegated book PDA being protected; the program signs as it.
pub struct PermissionCpiAccounts<'a> {
    pub payer: &'a AccountInfo,
    pub permissioned_account: &'a AccountInfo,
    pub permission: &'a AccountInfo,
    pub vault: &'a AccountInfo,
    pub magic_program: &'a AccountInfo,
    pub permission_program: &'a AccountInfo,
}

impl PermissionCpiAccounts<'_> {
    fn check_program(&self) -> ProgramResult {
        if self.permission_program.key() != &PERMISSION_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        Ok(())
    }
}

/// CREATE the ephemeral permission for `permissioned_account`. `data` is the
/// pre-serialized create payload (`write_ix_data(.., CREATE_DISCRIMINATOR, ..)`)
/// built in the caller's frame. The book PDA signs via `signer`.
pub fn cpi_create_permission(
    a: &PermissionCpiAccounts,
    data: &[u8],
    signer: &[Signer],
) -> ProgramResult {
    a.check_program()?;
    // CREATE account order: payer(w,s), permissioned(r,s), permission(w), vault(w), magic(r).
    let metas = [
        AccountMeta::new(a.payer.key(), true, true),
        AccountMeta::new(a.permissioned_account.key(), false, true),
        AccountMeta::new(a.permission.key(), true, false),
        AccountMeta::new(a.vault.key(), true, false),
        AccountMeta::new(a.magic_program.key(), false, false),
    ];
    let ix = Instruction { program_id: &PERMISSION_PROGRAM_ID, accounts: &metas, data };
    slice_invoke_signed(
        &ix,
        &[a.payer, a.permissioned_account, a.permission, a.vault, a.magic_program, a.permission_program],
        signer,
    )
}

/// UPDATE the permission. `data` is the pre-serialized update payload. The
/// protected PDA signs (appears as a readonly signer in the bespoke order).
pub fn cpi_update_permission(
    a: &PermissionCpiAccounts,
    data: &[u8],
    signer: &[Signer],
) -> ProgramResult {
    a.check_program()?;
    let metas = update_metas(a);
    let ix = Instruction { program_id: &PERMISSION_PROGRAM_ID, accounts: &metas, data };
    slice_invoke_signed(
        &ix,
        &[a.payer, a.permissioned_account, a.permission, a.vault, a.magic_program, a.permission_program],
        signer,
    )
}

/// CLOSE the permission, refunding rent to the payer (the book PDA).
pub fn cpi_close_permission(a: &PermissionCpiAccounts, signer: &[Signer]) -> ProgramResult {
    a.check_program()?;
    let data = CLOSE_DISCRIMINATOR.to_le_bytes();
    let metas = update_metas(a);
    let ix = Instruction { program_id: &PERMISSION_PROGRAM_ID, accounts: &metas, data: &data };
    slice_invoke_signed(
        &ix,
        &[a.payer, a.permissioned_account, a.permission, a.vault, a.magic_program, a.permission_program],
        signer,
    )
}

/// UPDATE/CLOSE account order: payer(w,s), authority=permissioned(r), permissioned(r,s),
/// permission(w), vault(w), magic(r). The PDA signs (authority is NOT a signer).
fn update_metas<'a>(a: &PermissionCpiAccounts<'a>) -> [AccountMeta<'a>; 6] {
    [
        AccountMeta::new(a.payer.key(), true, true),
        AccountMeta::new(a.permissioned_account.key(), false, false),
        AccountMeta::new(a.permissioned_account.key(), false, true),
        AccountMeta::new(a.permission.key(), true, false),
        AccountMeta::new(a.vault.key(), true, false),
        AccountMeta::new(a.magic_program.key(), false, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_values_and_read_set() {
        assert_eq!((AUTHORITY_FLAG, TX_LOGS_FLAG, TX_BALANCES_FLAG, TX_MESSAGE_FLAG, ACCOUNT_SIGNATURES_FLAG), (1, 2, 4, 8, 16));
        assert_eq!(MEMBER_READ_FLAGS, 14);
        assert_eq!(PERMISSION_SEED, b"permission:");
    }

    #[test]
    fn create_data_public_is_disc_plus_zero() {
        let mut buf = [0u8; MAX_DATA];
        let len = write_ix_data(&mut buf, CREATE_DISCRIMINATOR, false, &[]);
        assert_eq!(len, 9);
        assert_eq!(&buf[..len], &[6, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn update_data_private_one_member_byte_exact() {
        let mut buf = [0u8; MAX_DATA];
        let len = write_ix_data(&mut buf, UPDATE_DISCRIMINATOR, true, &[Member { flags: MEMBER_READ_FLAGS, pubkey: [7u8; 32] }]);
        assert_eq!(len, 8 + members_args_len(1));
        assert_eq!(&buf[0..8], &7u64.to_le_bytes()); // UPDATE disc = 7
        assert_eq!(buf[8], 1); // is_private
        assert_eq!(buf[9], 14); // MEMBER_READ_FLAGS
        assert_eq!(&buf[10..42], &[7u8; 32]);
    }

    #[test]
    fn two_members_serialize_in_order() {
        let mut buf = [0u8; MAX_DATA];
        let len = write_ix_data(&mut buf, CREATE_DISCRIMINATOR, true, &[
            Member { flags: AUTHORITY_FLAG | MEMBER_READ_FLAGS, pubkey: [1u8; 32] },
            Member { flags: TX_BALANCES_FLAG, pubkey: [2u8; 32] },
        ]);
        assert_eq!(len, 8 + 1 + 2 * 33);
        assert_eq!(buf[8], 1);
        assert_eq!(buf[9], 0b1111); // 15
        assert_eq!(&buf[10..42], &[1u8; 32]);
        assert_eq!(buf[42], TX_BALANCES_FLAG);
        assert_eq!(&buf[43..75], &[2u8; 32]);
    }

    #[test]
    fn close_data_is_just_discriminator() {
        assert_eq!(CLOSE_DISCRIMINATOR.to_le_bytes(), [8, 0, 0, 0, 0, 0, 0, 0]);
    }
}
