//! book_permission — PRIVATE / dark-pool book (TEE) management. Three thin
//! instructions over the hand-rolled MagicBlock permission CPIs in
//! `er_permission`: create the ephemeral permission for a DELEGATED book, toggle
//! its privacy + reader allow-list, and tear it down. Faithful port of the
//! Anchor `init_book_permission` / `set_book_privacy` / `close_book_permission`.
//!
//! All three share the `BookPermission` account set, authority-gated (the market
//! authority), with the delegated book PDA signing each CPI as itself.
//!
//! accounts: [authority (signer), market (program-owned, r),
//!            market_book (delegated PDA, w, signs), permission (permission-prog
//!            PDA, w), ephemeral_vault (w), magic_program, permission_program]

use crate::book::MARKET_BOOK_SEED;
use crate::er_permission::{
    cpi_close_permission, cpi_create_permission, cpi_update_permission, write_ix_data, Member,
    PermissionCpiAccounts, CREATE_DISCRIMINATOR, EPHEMERAL_VAULT_ID, MAGIC_PROGRAM_ID, MAX_DATA,
    MAX_PRIVACY_MEMBERS, MEMBER_READ_FLAGS, PERMISSION_PROGRAM_ID, PERMISSION_SEED,
    UPDATE_DISCRIMINATOR,
};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

/// Validate the shared BookPermission accounts and return the book PDA bump
/// (needed to sign the CPI as the book). Authority-gated; all helper PDAs pinned.
fn validate<'a>(
    pid: &Pubkey,
    accounts: &'a [AccountInfo],
) -> Result<(u8, PermissionCpiAccounts<'a>), ProgramError> {
    let [authority, market, market_book, permission, vault, magic_program, permission_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner); // only the market authority
        }
    }

    // The book PDA (canonical under OUR program — currently delegated). Bind it so
    // a caller can't substitute another account; the bump lets us sign as it.
    let bump = assert_pda(market_book, &[MARKET_BOOK_SEED, &market.key()[..]], pid)?;

    // The permission account is the PDA [PERMISSION_SEED, market_book] under the
    // MagicBlock permission program — pin it.
    let (expected_perm, _) =
        find_program_address(&[PERMISSION_SEED, &market_book.key()[..]], &PERMISSION_PROGRAM_ID);
    if permission.key() != &expected_perm {
        return Err(ProgramError::InvalidArgument);
    }
    // Address-pin the fixed MagicBlock accounts.
    if vault.key() != &EPHEMERAL_VAULT_ID
        || magic_program.key() != &MAGIC_PROGRAM_ID
        || permission_program.key() != &PERMISSION_PROGRAM_ID
    {
        return Err(ProgramError::InvalidArgument);
    }

    Ok((
        bump,
        PermissionCpiAccounts {
            payer: market_book,
            permissioned_account: market_book,
            permission,
            vault,
            magic_program,
            permission_program,
        },
    ))
}

/// init_book_permission — create the ephemeral permission (PUBLIC start).
/// Idempotent: a no-op if the permission account already exists.
pub fn init(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let (bump, a) = validate(pid, accounts)?;
    // Idempotent: already created.
    if a.permission.lamports() > 0 {
        return Ok(());
    }
    let mkt = &accounts[1];
    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&mkt.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];

    // Public start: is_private=false, no members.
    let mut data = [0u8; MAX_DATA];
    let len = write_ix_data(&mut data, CREATE_DISCRIMINATOR, false, &[]);
    cpi_create_permission(&a, &data[..len], &signer)
}

/// set_book_privacy — toggle privacy + set the reader allow-list.
/// data: [is_private u8][n_members u8][member_pubkey [u8;32] * n]
pub fn set_privacy(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let is_private = data[0] != 0;
    let n = data[1] as usize;
    if n > MAX_PRIVACY_MEMBERS {
        return Err(ProgramError::InvalidArgument);
    }
    if data.len() < 2 + n * 32 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let (bump, a) = validate(pid, accounts)?;
    let mkt = &accounts[1];
    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&mkt.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];

    // Build the member list (read-access for each) only when going private; a
    // public toggle clears the list. Members live in this (instruction) frame.
    let mut members = [Member { flags: 0, pubkey: [0u8; 32] }; MAX_PRIVACY_MEMBERS];
    let count = if is_private { n } else { 0 };
    for (i, slot) in members.iter_mut().enumerate().take(count) {
        let off = 2 + i * 32;
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&data[off..off + 32]);
        slot.flags = MEMBER_READ_FLAGS;
        slot.pubkey = pk;
    }

    let mut out = [0u8; MAX_DATA];
    let len = write_ix_data(&mut out, UPDATE_DISCRIMINATOR, is_private, &members[..count]);
    cpi_update_permission(&a, &out[..len], &signer)
}

/// close_book_permission — tear down the ephemeral permission, rent → book PDA.
pub fn close(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let (bump, a) = validate(pid, accounts)?;
    let mkt = &accounts[1];
    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_BOOK_SEED),
        Seed::from(&mkt.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_close_permission(&a, &signer)
}
