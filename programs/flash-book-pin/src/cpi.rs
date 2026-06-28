//! Cross-program-invocation scaffolding for the lifecycle / collateral
//! instructions (Phase 0 of the instruction port).
//!
//! Hand-rolled against bare `pinocchio` (no `pinocchio-token` / `-system`
//! dependency) to keep the crate's zero-dependency, zero-alloc footprint.
//!
//! Layout discipline: the ERROR-PRONE part — the System / SPL-Token
//! instruction-data byte layout — lives in pure `*_data()` encoders that are
//! unit-tested on the HOST. The `invoke*` wrappers require the Solana runtime
//! and so are compiled only for the SBF target (`#[cfg(target_os = "solana")]`)
//! and exercised under `build-sbf` / on-chain.
//!
//! SECURITY: callers MUST have validated account ownership / PDAs / signers via
//! [`crate::guard`] BEFORE invoking these — they move lamports / tokens and
//! create accounts; they do not themselves re-derive trust.

/// System program id — the all-zero pubkey (`11111111111111111111111111111111`).
pub const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// SPL Token program id (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`).
pub const TOKEN_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237,
    95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];

/// Associated-Token-Account program id
/// (`ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`). Base58 round-trip verified.
pub const ATA_PROGRAM_ID: [u8; 32] = [
    0x8C, 0x97, 0x25, 0x8F, 0x4E, 0x24, 0x89, 0xF1, 0xBB, 0x3D, 0x10, 0x29, 0x14, 0x8E, 0x0D, 0x83,
    0x0B, 0x5A, 0x13, 0x99, 0xDA, 0xFF, 0x10, 0x84, 0x04, 0x8E, 0x7B, 0xD8, 0xDB, 0xE9, 0xF8, 0x59,
];

// ─────────────────────────── instruction-data encoders ─────────────────────
// Pure, host-testable. These are the stable on-chain wire formats.

/// `SystemInstruction::CreateAccount` data: u32 tag `0`, then `lamports` (u64),
/// `space` (u64), and the new account `owner` (32 bytes). 52 bytes total.
#[inline]
pub fn create_account_data(lamports: u64, space: u64, owner: &[u8; 32]) -> [u8; 52] {
    let mut d = [0u8; 52];
    // d[0..4] = tag 0 (CreateAccount) — already zero.
    d[4..12].copy_from_slice(&lamports.to_le_bytes());
    d[12..20].copy_from_slice(&space.to_le_bytes());
    d[20..52].copy_from_slice(owner);
    d
}

/// `SystemInstruction::Transfer` data: u32 tag `2`, then `lamports` (u64).
#[inline]
pub fn system_transfer_data(lamports: u64) -> [u8; 12] {
    let mut d = [0u8; 12];
    d[0] = 2; // Transfer
    d[4..12].copy_from_slice(&lamports.to_le_bytes());
    d
}

/// `SystemInstruction::Allocate` data: u32 tag `8`, then `space` (u64).
#[inline]
pub fn allocate_data(space: u64) -> [u8; 12] {
    let mut d = [0u8; 12];
    d[0] = 8; // Allocate
    d[4..12].copy_from_slice(&space.to_le_bytes());
    d
}

/// `SystemInstruction::Assign` data: u32 tag `1`, then `owner` (32 bytes).
#[inline]
pub fn assign_data(owner: &[u8; 32]) -> [u8; 36] {
    let mut d = [0u8; 36];
    d[0] = 1; // Assign
    d[4..36].copy_from_slice(owner);
    d
}

/// `SplTokenInstruction::Transfer` data: tag byte `3`, then `amount` (u64).
#[inline]
pub fn transfer_data(amount: u64) -> [u8; 9] {
    let mut d = [0u8; 9];
    d[0] = 3; // Transfer
    d[1..9].copy_from_slice(&amount.to_le_bytes());
    d
}

/// `SplTokenInstruction::InitializeAccount3` data: tag byte `18`, then the
/// token account `owner` authority (32 bytes). Unlike v1 it does not require
/// the rent sysvar. The mint is supplied as an account, not in data.
#[inline]
pub fn init_account3_data(owner: &[u8; 32]) -> [u8; 33] {
    let mut d = [0u8; 33];
    d[0] = 18; // InitializeAccount3
    d[1..33].copy_from_slice(owner);
    d
}

/// SPL token account size in bytes (`spl_token::state::Account::LEN`).
pub const TOKEN_ACCOUNT_LEN: u64 = 165;

/// Byte offset of the `amount` field in an SPL token account: `mint` (32) +
/// `owner` (32). The field is a little-endian `u64`.
pub const TOKEN_AMOUNT_OFFSET: usize = 64;

/// Read the `amount` (balance) of an SPL token account from its raw data.
/// `Err(())` if the buffer is too short to be a token account — the caller must
/// also have checked the account is token-program-owned and the right length.
/// Pure + host-tested; works on a borrowed data slice with no CPI.
pub fn spl_token_amount(data: &[u8]) -> Result<u64, ()> {
    if data.len() < TOKEN_AMOUNT_OFFSET + 8 {
        return Err(());
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[TOKEN_AMOUNT_OFFSET..TOKEN_AMOUNT_OFFSET + 8]);
    Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod amount_tests {
    use super::{spl_token_amount, TOKEN_ACCOUNT_LEN, TOKEN_AMOUNT_OFFSET};

    #[test]
    fn reads_le_amount_at_offset_64() {
        let mut buf = [0u8; TOKEN_ACCOUNT_LEN as usize];
        buf[TOKEN_AMOUNT_OFFSET..TOKEN_AMOUNT_OFFSET + 8]
            .copy_from_slice(&123_456_789u64.to_le_bytes());
        assert_eq!(spl_token_amount(&buf), Ok(123_456_789));
    }

    #[test]
    fn max_amount_round_trips() {
        let mut buf = [0u8; TOKEN_ACCOUNT_LEN as usize];
        buf[TOKEN_AMOUNT_OFFSET..TOKEN_AMOUNT_OFFSET + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(spl_token_amount(&buf), Ok(u64::MAX));
    }

    #[test]
    fn short_buffer_is_rejected() {
        assert_eq!(spl_token_amount(&[0u8; TOKEN_AMOUNT_OFFSET + 7]), Err(()));
        assert_eq!(spl_token_amount(&[]), Err(()));
    }
}

// ─────────────────────────────── invoke wrappers (SBF) ─────────────────────

#[cfg(target_os = "solana")]
mod sol {
    use super::{
        allocate_data, assign_data, create_account_data, init_account3_data, system_transfer_data,
        transfer_data, ATA_PROGRAM_ID, SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID,
    };
    use pinocchio::{
        account_info::AccountInfo,
        cpi::{slice_invoke, slice_invoke_signed},
        instruction::{AccountMeta, Instruction, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        ProgramResult,
    };

    /// Create a PDA account, signed by its own `signer_seeds` (which MUST end
    /// with the canonical bump). `from` (rent payer) signs as a normal tx
    /// signer. `lamports` is the REQUIRED rent-exempt balance.
    ///
    /// HARDENED: if the address was pre-funded with lamports (a griefing vector
    /// — bare `CreateAccount` fails on a non-zero-lamport account, DoS'ing the
    /// creation), fall back to topup → allocate → assign, which always succeeds.
    #[allow(clippy::too_many_arguments)]
    pub fn create_pda_account(
        from: &AccountInfo,
        to: &AccountInfo,
        system_program: &AccountInfo,
        lamports: u64,
        space: u64,
        owner: &Pubkey,
        signer_seeds: &[Signer],
    ) -> ProgramResult {
        if system_program.key() != &SYSTEM_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }

        let current = to.lamports();
        if current == 0 {
            // Fast path: a single CreateAccount (fund + allocate + assign atomically).
            let data = create_account_data(lamports, space, owner);
            let metas = [
                AccountMeta::new(from.key(), true, true),
                AccountMeta::new(to.key(), true, true),
            ];
            let ix = Instruction {
                program_id: &SYSTEM_PROGRAM_ID,
                accounts: &metas,
                data: &data,
            };
            return slice_invoke_signed(&ix, &[from, to, system_program], signer_seeds);
        }

        // Pre-funded path: top up to rent-exemption, then allocate + assign.
        if current < lamports {
            let data = system_transfer_data(lamports - current);
            let metas = [
                AccountMeta::new(from.key(), true, true),
                AccountMeta::new(to.key(), true, false),
            ];
            let ix = Instruction {
                program_id: &SYSTEM_PROGRAM_ID,
                accounts: &metas,
                data: &data,
            };
            slice_invoke(&ix, &[from, to, system_program])?;
        }
        // Allocate `space` bytes (signed by the PDA).
        {
            let data = allocate_data(space);
            let metas = [AccountMeta::new(to.key(), true, true)];
            let ix = Instruction {
                program_id: &SYSTEM_PROGRAM_ID,
                accounts: &metas,
                data: &data,
            };
            slice_invoke_signed(&ix, &[to, system_program], signer_seeds)?;
        }
        // Assign to `owner` (signed by the PDA).
        {
            let data = assign_data(owner);
            let metas = [AccountMeta::new(to.key(), true, true)];
            let ix = Instruction {
                program_id: &SYSTEM_PROGRAM_ID,
                accounts: &metas,
                data: &data,
            };
            slice_invoke_signed(&ix, &[to, system_program], signer_seeds)?;
        }
        Ok(())
    }

    /// SPL-Token transfer where `authority` is a normal tx signer (trader
    /// depositing from their own ATA).
    pub fn token_transfer(
        token_program: &AccountInfo,
        source: &AccountInfo,
        destination: &AccountInfo,
        authority: &AccountInfo,
        amount: u64,
    ) -> ProgramResult {
        transfer_inner(token_program, source, destination, authority, amount, &[])
    }

    /// SPL-Token transfer where `authority` is a program-owned PDA, signed via
    /// `signer_seeds` (vault paying a withdrawal out).
    pub fn token_transfer_signed(
        token_program: &AccountInfo,
        source: &AccountInfo,
        destination: &AccountInfo,
        authority: &AccountInfo,
        amount: u64,
        signer_seeds: &[Signer],
    ) -> ProgramResult {
        transfer_inner(
            token_program,
            source,
            destination,
            authority,
            amount,
            signer_seeds,
        )
    }

    /// `InitializeAccount3` on a freshly-created (token-program-owned) account,
    /// setting its mint + `authority`. The account must already exist with
    /// `TOKEN_ACCOUNT_LEN` bytes owned by the token program (see
    /// `create_pda_account(.., owner = TOKEN_PROGRAM_ID, ..)`).
    pub fn init_token_account(
        token_program: &AccountInfo,
        account: &AccountInfo,
        mint: &AccountInfo,
        authority: &Pubkey,
    ) -> ProgramResult {
        if token_program.key() != &TOKEN_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        let data = init_account3_data(authority);
        let metas = [
            AccountMeta::new(account.key(), true, false),
            AccountMeta::new(mint.key(), false, false),
        ];
        let ix = Instruction {
            program_id: &TOKEN_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        };
        slice_invoke(&ix, &[account, mint, token_program])
    }

    /// Create the wallet's associated token account for `mint` via the ATA
    /// program's `CreateIdempotent` (tag byte `1`; a no-op if the ATA already
    /// exists). `payer` signs as a normal tx signer and funds the rent. NO tokens
    /// move — this creates an EMPTY token account. The `ata` address must be the
    /// canonical ATA the program derives; the ATA program rejects a mismatch.
    pub fn create_idempotent_ata(
        ata_program: &AccountInfo,
        payer: &AccountInfo,
        ata: &AccountInfo,
        wallet: &AccountInfo,
        mint: &AccountInfo,
        system_program: &AccountInfo,
        token_program: &AccountInfo,
    ) -> ProgramResult {
        if ata_program.key() != &ATA_PROGRAM_ID
            || system_program.key() != &SYSTEM_PROGRAM_ID
            || token_program.key() != &TOKEN_PROGRAM_ID
        {
            return Err(ProgramError::IncorrectProgramId);
        }
        let metas = [
            AccountMeta::new(payer.key(), true, true),
            AccountMeta::new(ata.key(), true, false),
            AccountMeta::new(wallet.key(), false, false),
            AccountMeta::new(mint.key(), false, false),
            AccountMeta::new(system_program.key(), false, false),
            AccountMeta::new(token_program.key(), false, false),
        ];
        let ix = Instruction {
            program_id: &ATA_PROGRAM_ID,
            accounts: &metas,
            data: &[1u8], // AssociatedTokenAccountInstruction::CreateIdempotent
        };
        slice_invoke(
            &ix,
            &[payer, ata, wallet, mint, system_program, token_program, ata_program],
        )
    }

    /// System-program lamport transfer: `from` (a normal tx signer) → `to`. Used
    /// to top up an account's rent-exempt balance when growing it.
    pub fn system_transfer(
        system_program: &AccountInfo,
        from: &AccountInfo,
        to: &AccountInfo,
        lamports: u64,
    ) -> ProgramResult {
        if system_program.key() != &SYSTEM_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        let data = system_transfer_data(lamports);
        let metas = [
            AccountMeta::new(from.key(), true, true),
            AccountMeta::new(to.key(), true, false),
        ];
        let ix = Instruction {
            program_id: &SYSTEM_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        };
        slice_invoke(&ix, &[from, to, system_program])
    }

    /// SPL-Token `CloseAccount` (tag byte `9`): close `account` and send its rent
    /// lamports to `destination`. `authority` (the token account's owner) signs.
    /// The token program ENFORCES that the account balance is 0 and that
    /// `authority` is the owner — so NO token value can move and only the owner's
    /// own account can be closed.
    pub fn close_token_account(
        token_program: &AccountInfo,
        account: &AccountInfo,
        destination: &AccountInfo,
        authority: &AccountInfo,
    ) -> ProgramResult {
        if token_program.key() != &TOKEN_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        let metas = [
            AccountMeta::new(account.key(), true, false),
            AccountMeta::new(destination.key(), true, false),
            AccountMeta::new(authority.key(), false, true),
        ];
        let ix = Instruction {
            program_id: &TOKEN_PROGRAM_ID,
            accounts: &metas,
            data: &[9u8], // SplTokenInstruction::CloseAccount
        };
        slice_invoke(&ix, &[account, destination, authority, token_program])
    }

    fn transfer_inner(
        token_program: &AccountInfo,
        source: &AccountInfo,
        destination: &AccountInfo,
        authority: &AccountInfo,
        amount: u64,
        signer_seeds: &[Signer],
    ) -> ProgramResult {
        if token_program.key() != &TOKEN_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        let data = transfer_data(amount);
        let metas = [
            AccountMeta::new(source.key(), true, false),
            AccountMeta::new(destination.key(), true, false),
            AccountMeta::new(authority.key(), false, signer_seeds.is_empty()),
        ];
        let ix = Instruction {
            program_id: &TOKEN_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        };
        let infos = [source, destination, authority, token_program];
        if signer_seeds.is_empty() {
            slice_invoke(&ix, &infos)
        } else {
            slice_invoke_signed(&ix, &infos, signer_seeds)
        }
    }
}

#[cfg(target_os = "solana")]
pub use sol::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_account_data_layout() {
        let owner = [7u8; 32];
        let d = create_account_data(1_000_000, 200, &owner);
        assert_eq!(&d[0..4], &[0, 0, 0, 0], "tag 0 = CreateAccount");
        assert_eq!(u64::from_le_bytes(d[4..12].try_into().unwrap()), 1_000_000);
        assert_eq!(u64::from_le_bytes(d[12..20].try_into().unwrap()), 200);
        assert_eq!(&d[20..52], &owner);
    }

    #[test]
    fn transfer_data_layout() {
        let d = transfer_data(42_000);
        assert_eq!(d[0], 3, "tag 3 = Transfer");
        assert_eq!(u64::from_le_bytes(d[1..9].try_into().unwrap()), 42_000);
    }

    #[test]
    fn init_account3_data_layout() {
        let owner = [3u8; 32];
        let d = init_account3_data(&owner);
        assert_eq!(d[0], 18, "tag 18 = InitializeAccount3");
        assert_eq!(&d[1..33], &owner);
        assert_eq!(TOKEN_ACCOUNT_LEN, 165);
    }

    #[test]
    fn system_instruction_encoders() {
        let t = system_transfer_data(5_000);
        assert_eq!(&t[0..4], &[2, 0, 0, 0], "tag 2 = system Transfer");
        assert_eq!(u64::from_le_bytes(t[4..12].try_into().unwrap()), 5_000);

        let a = allocate_data(200);
        assert_eq!(&a[0..4], &[8, 0, 0, 0], "tag 8 = Allocate");
        assert_eq!(u64::from_le_bytes(a[4..12].try_into().unwrap()), 200);

        let owner = [9u8; 32];
        let s = assign_data(&owner);
        assert_eq!(&s[0..4], &[1, 0, 0, 0], "tag 1 = Assign");
        assert_eq!(&s[4..36], &owner);
    }

    #[test]
    fn program_ids_are_canonical() {
        assert_eq!(SYSTEM_PROGRAM_ID, [0u8; 32]);
        // First/last bytes of TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA.
        assert_eq!(TOKEN_PROGRAM_ID[0], 6);
        assert_eq!(TOKEN_PROGRAM_ID[31], 169);
    }
}
