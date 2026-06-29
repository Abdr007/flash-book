//! MagicBlock Ephemeral Rollups (ER) shared constants.
//!
//! The `ephemeral-rollups-sdk` cannot be added to this crate (its bytemuck/borsh
//! pins conflict with pyth + the port's deps), so every ER interaction is
//! hand-rolled. This module holds the constants those hand-rolled paths share;
//! the delegate/commit/undelegate CPIs build on it.

use pinocchio::{
    account_info::AccountInfo,
    cpi::slice_invoke_signed,
    instruction::{AccountMeta, Instruction, Signer},
    program_error::ProgramError,
    ProgramResult,
};

/// The MagicBlock delegation program (base58
/// `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`). A delegated account is
/// re-owned by this program while it runs on the ER; ownership by it is the
/// proof that an account is currently delegated. Round-trip-verified.
pub const DELEGATION_PROGRAM_ID: [u8; 32] = [
    181, 183, 0, 225, 242, 87, 58, 192, 204, 6, 34, 1, 52, 74, 207, 151, 184, 53, 6, 235, 140, 229,
    25, 152, 204, 98, 126, 24, 147, 128, 167, 62,
];

/// PDA seed tags (verbatim from the delegation program API).
pub const DELEGATE_BUFFER_TAG: &[u8] = b"buffer";
pub const DELEGATION_RECORD_TAG: &[u8] = b"delegation";
pub const DELEGATION_METADATA_TAG: &[u8] = b"delegation-metadata";

/// Instruction discriminators of the delegation program.
pub const DELEGATE_DISCRIMINATOR: u8 = 0;
pub const UNDELEGATE_DISCRIMINATOR: u8 = 3;

/// Max serialized Delegate ix data for our use (disc + commit_freq + ≤4 seeds of
/// ≤40 bytes each + Option<Pubkey>). A market_book delegation uses 3 seeds
/// (prefix + 32-byte market key + 1 bump), well under this.
pub const MAX_DELEGATE_DATA: usize = 1 + 4 + 4 + 4 * (4 + 40) + 1 + 32;

/// Borsh-serialize the Delegate ix data into `buf`, returning the length:
/// `[disc:u8][commit_frequency_ms:u32 LE][seeds: Vec<Vec<u8>>][validator: Option<Pubkey>]`.
/// `Vec<Vec<u8>>` borsh = `len:u32` then each `len:u32 ++ bytes`; `Option` borsh =
/// `0` (None) or `1 ++ 32 bytes` (Some). Byte-identical to the API's `DelegateArgs`.
pub fn write_delegate_data(
    buf: &mut [u8],
    commit_frequency_ms: u32,
    seeds: &[&[u8]],
    validator: Option<&[u8; 32]>,
) -> usize {
    let mut o = 0;
    buf[o] = DELEGATE_DISCRIMINATOR;
    o += 1;
    buf[o..o + 4].copy_from_slice(&commit_frequency_ms.to_le_bytes());
    o += 4;
    buf[o..o + 4].copy_from_slice(&(seeds.len() as u32).to_le_bytes());
    o += 4;
    for s in seeds {
        buf[o..o + 4].copy_from_slice(&(s.len() as u32).to_le_bytes());
        o += 4;
        buf[o..o + s.len()].copy_from_slice(s);
        o += s.len();
    }
    match validator {
        None => {
            buf[o] = 0;
            o += 1;
        }
        Some(v) => {
            buf[o] = 1;
            o += 1;
            buf[o..o + 32].copy_from_slice(v);
            o += 32;
        }
    }
    o
}

#[cfg(test)]
mod delegate_tests {
    use super::*;

    #[test]
    fn delegate_data_no_validator_byte_exact() {
        // commit_frequency_ms = 30_000; seeds = [b"book", market(2 bytes for test)];
        // validator = None.
        let mut buf = [0u8; MAX_DELEGATE_DATA];
        let market = [0xAB, 0xCD];
        let len = write_delegate_data(&mut buf, 30_000, &[b"book", &market], None);
        // disc(1) + freq(4) + count(4) + [len(4)+4] + [len(4)+2] + None(1)
        assert_eq!(len, 1 + 4 + 4 + (4 + 4) + (4 + 2) + 1);
        assert_eq!(buf[0], 0); // DELEGATE disc
        assert_eq!(&buf[1..5], &30_000u32.to_le_bytes());
        assert_eq!(&buf[5..9], &2u32.to_le_bytes()); // 2 seeds
        assert_eq!(&buf[9..13], &4u32.to_le_bytes()); // first seed len 4
        assert_eq!(&buf[13..17], b"book");
        assert_eq!(&buf[17..21], &2u32.to_le_bytes()); // second seed len 2
        assert_eq!(&buf[21..23], &market);
        assert_eq!(buf[23], 0); // None
    }

    #[test]
    fn delegate_data_with_validator() {
        let mut buf = [0u8; MAX_DELEGATE_DATA];
        let v = [9u8; 32];
        let len = write_delegate_data(&mut buf, 0, &[b"x"], Some(&v));
        // disc(1)+freq(4)+count(4)+[4+1]+Some(1+32)
        assert_eq!(len, 1 + 4 + 4 + 5 + 33);
        assert_eq!(buf[len - 33], 1); // Some tag
        assert_eq!(&buf[len - 32..len], &v);
    }

    #[test]
    fn tags_and_discriminators() {
        assert_eq!(DELEGATE_BUFFER_TAG, b"buffer");
        assert_eq!(DELEGATION_RECORD_TAG, b"delegation");
        assert_eq!(DELEGATION_METADATA_TAG, b"delegation-metadata");
        assert_eq!((DELEGATE_DISCRIMINATOR, UNDELEGATE_DISCRIMINATOR), (0, 3));
    }
}


pub struct DelegateAccounts<'a> {
    pub payer: &'a AccountInfo,
    pub delegated_account: &'a AccountInfo,
    pub owner_program: &'a AccountInfo,
    pub delegate_buffer: &'a AccountInfo,
    pub delegation_record: &'a AccountInfo,
    pub delegation_metadata: &'a AccountInfo,
    pub system_program: &'a AccountInfo,
    pub delegation_program: &'a AccountInfo,
}

/// CPI the delegation program's Delegate ix. `data` is pre-built via
/// `write_delegate_data`. The delegated PDA signs via `signer`.
pub fn cpi_delegate(a: &DelegateAccounts, data: &[u8], signer: &[Signer]) -> ProgramResult {
    if a.delegation_program.key() != &DELEGATION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let metas = [
        AccountMeta::new(a.payer.key(), true, true),
        AccountMeta::new(a.delegated_account.key(), true, true), // signer-by-PDA
        AccountMeta::new(a.owner_program.key(), false, false),
        AccountMeta::new(a.delegate_buffer.key(), true, false),
        AccountMeta::new(a.delegation_record.key(), true, false),
        AccountMeta::new(a.delegation_metadata.key(), true, false),
        AccountMeta::new(a.system_program.key(), false, false),
    ];
    let ix = Instruction { program_id: &DELEGATION_PROGRAM_ID, accounts: &metas, data };
    slice_invoke_signed(
        &ix,
        &[a.payer, a.delegated_account, a.owner_program, a.delegate_buffer, a.delegation_record, a.delegation_metadata, a.system_program, a.delegation_program],
        signer,
    )
}

pub struct UndelegateAccounts<'a> {
    pub payer: &'a AccountInfo,
    pub delegated_account: &'a AccountInfo,
    pub owner_program: &'a AccountInfo,
    pub buffer: &'a AccountInfo,
    pub system_program: &'a AccountInfo,
    pub delegation_program: &'a AccountInfo,
}

/// CPI the delegation program's Undelegate ix (data = single disc byte 3).
pub fn cpi_undelegate(a: &UndelegateAccounts, signer: &[Signer]) -> ProgramResult {
    if a.delegation_program.key() != &DELEGATION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let data = [UNDELEGATE_DISCRIMINATOR];
    let metas = [
        AccountMeta::new(a.payer.key(), true, true),
        AccountMeta::new(a.delegated_account.key(), true, true),
        AccountMeta::new(a.owner_program.key(), false, false),
        AccountMeta::new(a.buffer.key(), true, false),
        AccountMeta::new(a.system_program.key(), false, false),
    ];
    let ix = Instruction { program_id: &DELEGATION_PROGRAM_ID, accounts: &metas, data: &data };
    slice_invoke_signed(
        &ix,
        &[a.payer, a.delegated_account, a.owner_program, a.buffer, a.system_program, a.delegation_program],
        signer,
    )
}
