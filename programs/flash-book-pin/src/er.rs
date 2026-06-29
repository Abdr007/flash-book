//! MagicBlock Ephemeral Rollups (ER) shared constants.
//!
//! The `ephemeral-rollups-sdk` cannot be added to this crate (its bytemuck/borsh
//! pins conflict with pyth + the port's deps), so every ER interaction is
//! hand-rolled. This module holds the constants those hand-rolled paths share;
//! the delegate/commit/undelegate CPIs build on it.

use pinocchio::{
    account_info::AccountInfo,
    cpi::slice_invoke,
    instruction::{AccountMeta, Instruction, Signer},
    program_error::ProgramError,
    ProgramResult,
};

/// MagicBlock Magic (validator) program — runs ON the ER (same id as the
/// permission module's). The commit CPIs target this program.
pub use crate::er_permission::MAGIC_PROGRAM_ID;

/// MagicBlock Magic context account (`MagicContext111…`, writable; collects
/// scheduled commits). Round-trip-verified.
pub const MAGIC_CONTEXT_ID: [u8; 32] = [
    5, 69, 180, 36, 196, 165, 40, 191, 95, 180, 3, 47, 68, 82, 130, 142, 187, 56, 171, 193, 210,
    220, 151, 247, 63, 139, 148, 84, 128, 0, 0, 0,
];

/// `MagicBlockInstruction` enum tags (bincode u32 LE).
const SCHEDULE_COMMIT: [u8; 4] = [1, 0, 0, 0];
const SCHEDULE_COMMIT_AND_UNDELEGATE: [u8; 4] = [2, 0, 0, 0];

/// ON-THE-ER: schedule a commit of the `committed` account's state back to the
/// base layer; if `allow_undelegation`, also queue undelegation
/// (`ScheduleCommitAndUndelegate`). Faithful port of the Anchor `er::cpi_commit`
/// for a single committed account (our only use). Plain `invoke` — the payer
/// signs; no PDA seeds. Account order: `[payer(s,w), magic_context(w), committed]`.
pub fn cpi_commit(
    payer: &AccountInfo,
    magic_context: &AccountInfo,
    magic_program: &AccountInfo,
    committed: &AccountInfo,
    allow_undelegation: bool,
) -> ProgramResult {
    if magic_program.key() != &MAGIC_PROGRAM_ID || magic_context.key() != &MAGIC_CONTEXT_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let data = if allow_undelegation {
        SCHEDULE_COMMIT_AND_UNDELEGATE
    } else {
        SCHEDULE_COMMIT
    };
    let metas = [
        AccountMeta::new(payer.key(), true, true),
        AccountMeta::new(magic_context.key(), true, false),
        AccountMeta::new(committed.key(), committed.is_writable(), committed.is_signer()),
    ];
    let ix = Instruction { program_id: &MAGIC_PROGRAM_ID, accounts: &metas, data: &data };
    slice_invoke(&ix, &[payer, magic_context, committed, magic_program])
}

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

/// Max serialized Delegate ix data for our use (8-byte disc + commit_freq + ≤4
/// seeds of ≤40 bytes each + Option<Pubkey>). A market_book delegation uses 2
/// seeds (prefix + 32-byte market key — NO bump in args.seeds), well under this.
pub const MAX_DELEGATE_DATA: usize = 8 + 4 + 4 + 4 * (4 + 40) + 1 + 32;

/// Borsh-serialize the Delegate ix data into `buf`, returning the length:
/// `[disc:[u8;8]][commit_frequency_ms:u32 LE][seeds: Vec<Vec<u8>>][validator: Option<Pubkey>]`.
/// WAVE-24i: the upgraded DLP uses the FAST path — an 8-BYTE discriminator block
/// (`split_at(8)`; byte[0] = Delegate = 0) followed by the borsh `DelegateArgs`.
/// `Vec<Vec<u8>>` borsh = `len:u32` then each `len:u32 ++ bytes`; `Option` borsh =
/// `0` (None) or `1 ++ 32 bytes` (Some). Byte-identical to the API's `DelegateArgs`.
pub fn write_delegate_data(
    buf: &mut [u8],
    commit_frequency_ms: u32,
    seeds: &[&[u8]],
    validator: Option<&[u8; 32]>,
) -> usize {
    let mut o = 0;
    // WAVE-24i fast path: 8-byte discriminator block (byte[0] = Delegate = 0).
    buf[o..o + 8].copy_from_slice(&[DELEGATE_DISCRIMINATOR, 0, 0, 0, 0, 0, 0, 0]);
    o += 8;
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
        // disc(8) + freq(4) + count(4) + [len(4)+4] + [len(4)+2] + None(1)
        assert_eq!(len, 8 + 4 + 4 + (4 + 4) + (4 + 2) + 1);
        assert_eq!(&buf[0..8], &[0u8; 8]); // 8-byte DELEGATE disc (byte[0]=0)
        assert_eq!(&buf[8..12], &30_000u32.to_le_bytes());
        assert_eq!(&buf[12..16], &2u32.to_le_bytes()); // 2 seeds (NO bump)
        assert_eq!(&buf[16..20], &4u32.to_le_bytes()); // first seed len 4
        assert_eq!(&buf[20..24], b"book");
        assert_eq!(&buf[24..28], &2u32.to_le_bytes()); // second seed len 2
        assert_eq!(&buf[28..30], &market);
        assert_eq!(buf[30], 0); // None
    }

    #[test]
    fn delegate_data_with_validator() {
        let mut buf = [0u8; MAX_DELEGATE_DATA];
        let v = [9u8; 32];
        let len = write_delegate_data(&mut buf, 0, &[b"x"], Some(&v));
        // disc(8)+freq(4)+count(4)+[4+1]+Some(1+32)
        assert_eq!(len, 8 + 4 + 4 + 5 + 33);
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

/// CPI the delegation program's Delegate ix (WAVE-24i fast path). `data` is the
/// pre-built `[8-byte disc][DelegateArgs]` (see `write_delegate_data`); `signer`
/// is the delegated PDA's seeds INCLUDING its bump.
///
/// The upgraded DLP requires the CALLER to STAGE the account before the CPI (the
/// old DLP did this internally): create an owner-program buffer PDA, copy the
/// account into it, zero the account, and hand its ownership to the delegation
/// program. The DLP copies the buffer back into the (zeroed) account during its
/// CPI, so the round-trip is lossless. Faithful port of the Anchor `cpi_delegate`
/// (= `ephemeral_rollups_sdk::cpi::delegate_account`).
///
/// SECURITY: the caller MUST have verified `delegated_account` is program-owned
/// and `owner_program == this program` before calling.
#[cfg(target_os = "solana")]
pub fn cpi_delegate(a: &DelegateAccounts, data: &[u8], signer: &[Signer]) -> ProgramResult {
    use crate::cpi::{assign_data, create_pda_account, SYSTEM_PROGRAM_ID};
    use pinocchio::cpi::slice_invoke_signed;
    use pinocchio::instruction::Seed;
    use pinocchio::pubkey::find_program_address;
    use pinocchio::sysvars::{rent::Rent, Sysvar};

    if a.delegation_program.key() != &DELEGATION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let data_len = a.delegated_account.data_len();
    let (buffer_pda, buffer_bump) = find_program_address(
        &[DELEGATE_BUFFER_TAG, &a.delegated_account.key()[..]],
        a.owner_program.key(),
    );
    if a.delegate_buffer.key() != &buffer_pda {
        return Err(ProgramError::InvalidArgument);
    }

    // 1. Create the buffer PDA (owned by owner_program), sized to the account.
    let bb = [buffer_bump];
    let bseeds = [
        Seed::from(DELEGATE_BUFFER_TAG),
        Seed::from(&a.delegated_account.key()[..]),
        Seed::from(&bb[..]),
    ];
    let bsigner = [Signer::from(&bseeds[..])];
    let lamports = Rent::get()?.minimum_balance(data_len);
    create_pda_account(
        a.payer, a.delegate_buffer, a.system_program,
        lamports, data_len as u64, a.owner_program.key(), &bsigner,
    )?;

    // 2. Stage the account's data into the buffer.
    {
        let src = a.delegated_account.try_borrow_data()?;
        let mut dst = a.delegate_buffer.try_borrow_mut_data()?;
        if dst.len() != src.len() {
            return Err(ProgramError::InvalidAccountData);
        }
        dst.copy_from_slice(&src);
    }
    // 3. Zero the account (required before its ownership can be handed off).
    {
        let mut d = a.delegated_account.try_borrow_mut_data()?;
        d.fill(0);
    }
    // 4. Hand ownership to the delegation program: reassign the (now-zeroed) account
    //    we own directly to System, then System::assign → DLP under the PDA seeds.
    if !a.delegated_account.is_owned_by(&SYSTEM_PROGRAM_ID) {
        unsafe { a.delegated_account.assign(&SYSTEM_PROGRAM_ID); }
    }
    if !a.delegated_account.is_owned_by(&DELEGATION_PROGRAM_ID) {
        let adata = assign_data(&DELEGATION_PROGRAM_ID);
        let metas = [AccountMeta::new(a.delegated_account.key(), true, true)];
        let ix = Instruction { program_id: &SYSTEM_PROGRAM_ID, accounts: &metas, data: &adata };
        slice_invoke_signed(&ix, &[a.delegated_account, a.system_program], signer)?;
    }

    // 5. CPI the Delegate ix (8-byte disc + DelegateArgs already in `data`). Account
    //    order/flags mirror the SDK; the delegated PDA signs via `signer`.
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
    )?;

    // 6. Close the now-consumed buffer, refunding rent to the payer (the runtime
    //    reclaims the zero-lamport account at the end of the instruction).
    let buf_lamports = a.delegate_buffer.lamports();
    unsafe {
        let pl = a.payer.borrow_mut_lamports_unchecked();
        *pl = (*pl).checked_add(buf_lamports).ok_or(ProgramError::ArithmeticOverflow)?;
        *a.delegate_buffer.borrow_mut_lamports_unchecked() = 0;
    }
    Ok(())
}

/// Host compile-stub: the staging CPI requires the Solana runtime. Host unit tests
/// exercise `write_delegate_data` (the byte layout); the staging is validated under
/// build-sbf / on-chain (and ultimately the live ER acceptance run).
#[cfg(not(target_os = "solana"))]
pub fn cpi_delegate(_a: &DelegateAccounts, _data: &[u8], _signer: &[Signer]) -> ProgramResult {
    Err(ProgramError::Custom(0xE0DE)) // ER delegate CPI unavailable off-chain
}

// NOTE (re-audit 2026-06-30): the program-initiated `cpi_undelegate` (+ its
// `UndelegateAccounts`) was REMOVED to match the Anchor reference. The upgraded
// delegation program makes undelegation VALIDATOR-driven: the owner schedules it
// ON the ER via `commit_and_undelegate_*` (`cpi_commit(allow_undelegation=true)`)
// and the DLP later invokes `process_undelegation` (the EXTERNAL_UNDELEGATE
// callback) on the base layer. A program-issued Undelegate CPI (disc byte 3) is no
// longer a valid DLP entrypoint and is guaranteed to fail; `UNDELEGATE_DISCRIMINATOR`
// is retained only for documentation. `force_undelegate_market_book` therefore
// returns `OwnerForceUndelegateUnavailable` after its liveness gate (no CPI).

#[inline]
fn max3(a: u64, b: u64, c: u64) -> u64 {
    let ab = if a > b { a } else { b };
    if ab > c { ab } else { c }
}

/// Whether the permissionless force-undelegate escape may fire. Faithful port of
/// the Anchor `er::force_undelegate_allowed` (Kani-proven F2/F3). Opens when:
///   • FAST: the most recent ER liveness signal of ANY kind (committed fill /
///     heartbeat / delegation baseline) is older than `stall_timeout_slots` — a
///     dead ER. Heartbeat-aware, so a quiet-but-heartbeating market isn't griefed.
///   • BACKSTOP: SETTLEMENT (a committed fill) has not advanced for the much
///     longer `censorship_timeout_slots`, IGNORING heartbeats — catches an
///     alive-but-censoring sequencer that heartbeats but settles nothing.
/// A zero baseline (never delegated via the upgraded path / never stamped) is
/// never escapable.
#[inline]
pub fn force_undelegate_allowed(
    current_slot: u64,
    last_mark_update_slot: u64,
    last_heartbeat_slot: u64,
    book_delegated_at_slot: u64,
    stall_timeout_slots: u64,
    censorship_timeout_slots: u64,
) -> bool {
    let er_baseline = max3(last_mark_update_slot, last_heartbeat_slot, book_delegated_at_slot);
    let er_stalled =
        er_baseline != 0 && current_slot.saturating_sub(er_baseline) > stall_timeout_slots;

    let settle_baseline = if last_mark_update_slot > book_delegated_at_slot {
        last_mark_update_slot
    } else {
        book_delegated_at_slot
    };
    let censored = settle_baseline != 0
        && current_slot.saturating_sub(settle_baseline) > censorship_timeout_slots;

    er_stalled || censored
}

#[cfg(test)]
mod force_undelegate_tests {
    use super::*;

    const STALL: u64 = 750;
    const CENSOR: u64 = 9_000;

    #[test]
    fn live_er_cannot_be_force_undelegated() {
        // delegated at 1000, a fill 50 slots ago (current 1100) → live → false.
        assert!(!force_undelegate_allowed(1_100, 1_050, 0, 1_000, STALL, CENSOR));
        // heartbeating but no fills, within stall → still false (not griefable).
        assert!(!force_undelegate_allowed(1_500, 0, 1_400, 1_000, STALL, CENSOR));
    }

    #[test]
    fn dead_er_opens_after_stall_timeout() {
        // delegated at 1000, no liveness; current = 1000 + 751 → stalled → true.
        assert!(force_undelegate_allowed(1_751, 0, 0, 1_000, STALL, CENSOR));
        // exactly at the timeout (not strictly greater) → still false.
        assert!(!force_undelegate_allowed(1_750, 0, 0, 1_000, STALL, CENSOR));
    }

    #[test]
    fn censoring_sequencer_opens_via_backstop() {
        // Heartbeats keep er_baseline fresh (no fast-path), but settlement
        // (last_mark_update) stalled past the censorship backstop → true.
        let delegated = 1_000;
        let current = delegated + CENSOR + 1;
        // Fresh heartbeat (1 slot ago) keeps the fast path closed, but settlement
        // (last_mark_update == delegated baseline) is stale past the censorship
        // backstop → the escape still opens.
        assert!(force_undelegate_allowed(current, delegated, current - 1, delegated, STALL, CENSOR));
    }

    #[test]
    fn zero_baseline_never_escapable() {
        assert!(!force_undelegate_allowed(1_000_000, 0, 0, 0, STALL, CENSOR));
    }
}

#[cfg(kani)]
mod force_undelegate_proofs {
    use super::*;

    /// F2/F3: the escape NEVER fires while the ER is live — if the most recent
    /// liveness signal is within the stall window AND settlement is within the
    /// censorship window, the gate stays closed. So a stalled/censoring ER is a
    /// necessary precondition; a healthy venue cannot be griefed.
    #[kani::proof]
    fn proof_never_fires_while_live() {
        let current: u64 = kani::any();
        let fill: u64 = kani::any();
        let heartbeat: u64 = kani::any();
        let delegated: u64 = kani::any();
        let stall: u64 = kani::any();
        let censor: u64 = kani::any();

        let er_baseline = max3(fill, heartbeat, delegated);
        let settle_baseline = if fill > delegated { fill } else { delegated };

        // "Live": most recent signal within stall AND settlement within censorship.
        let live = current.saturating_sub(er_baseline) <= stall
            && current.saturating_sub(settle_baseline) <= censor;

        if live {
            assert!(!force_undelegate_allowed(current, fill, heartbeat, delegated, stall, censor));
        }
    }
}


/// Max PDA seeds the undelegation callback handles (market = 3, book = 2).
pub const MAX_UNDELEGATE_SEEDS: usize = 4;

/// 8-byte discriminator the MagicBlock delegation program sends when it CPIs BACK
/// into this program to finalize an undelegation: `sha256("global:process_undelegation")[..8]`.
/// pin's entrypoint detects this prefix and routes to process_undelegation.
pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR: [u8; 8] = [196, 28, 41, 206, 48, 37, 51, 167];

/// Parse a borsh `Vec<Vec<u8>>` (the delegated account's PDA seeds) from `data`
/// into `out` (slices borrow `data`). Layout: `[count u32 LE]` then each
/// `[len u32 LE][bytes]`. Returns the seed count, or None on malformed / too many.
pub fn parse_undelegate_seeds<'a>(
    data: &'a [u8],
    out: &mut [&'a [u8]; MAX_UNDELEGATE_SEEDS],
) -> Option<usize> {
    if data.len() < 4 {
        return None;
    }
    let count = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if count > MAX_UNDELEGATE_SEEDS {
        return None;
    }
    let mut o = 4usize;
    for slot in out.iter_mut().take(count) {
        if data.len() < o + 4 {
            return None;
        }
        let len = u32::from_le_bytes(data[o..o + 4].try_into().ok()?) as usize;
        o += 4;
        if data.len() < o + len {
            return None;
        }
        *slot = &data[o..o + len];
        o += len;
    }
    Some(count)
}

#[cfg(test)]
mod undelegate_seed_tests {
    use super::*;

    #[test]
    fn parses_two_seeds() {
        // count=2; seed0 = b"book" (4); seed1 = [0xAA;3].
        let mut d = Vec::new();
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&4u32.to_le_bytes());
        d.extend_from_slice(b"book");
        d.extend_from_slice(&3u32.to_le_bytes());
        d.extend_from_slice(&[0xAA; 3]);
        let mut out: [&[u8]; MAX_UNDELEGATE_SEEDS] = [&[]; MAX_UNDELEGATE_SEEDS];
        assert_eq!(parse_undelegate_seeds(&d, &mut out), Some(2));
        assert_eq!(out[0], b"book");
        assert_eq!(out[1], &[0xAA, 0xAA, 0xAA]);
    }

    #[test]
    fn rejects_too_many_seeds() {
        let mut d = Vec::new();
        d.extend_from_slice(&5u32.to_le_bytes()); // 5 > MAX 4
        assert_eq!(parse_undelegate_seeds(&d, &mut [&[]; MAX_UNDELEGATE_SEEDS]), None);
    }

    #[test]
    fn rejects_truncated() {
        let mut d = Vec::new();
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&10u32.to_le_bytes()); // claims 10 bytes, none follow
        assert_eq!(parse_undelegate_seeds(&d, &mut [&[]; MAX_UNDELEGATE_SEEDS]), None);
    }

    #[test]
    fn disc_matches_anchor() {
        // sha256("global:process_undelegation")[..8]
        assert_eq!(EXTERNAL_UNDELEGATE_DISCRIMINATOR, [196, 28, 41, 206, 48, 37, 51, 167]);
    }
}


/// Finalize an undelegation initiated on the ER. Re-opens the delegated PDA under
/// THIS program (sized to the committed buffer) and copies the committed state
/// back. Faithful port of the Anchor `er::process_external_undelegate`.
///
/// Only the delegation program's SIGNED buffer may drive this (it is the signer
/// + owned by the delegation program). The target PDA is re-derived from the
/// passed seeds under our program and must match the account.
///
/// `solana`-only: it uses the create-account CPI (not available on the host
/// target where the unit tests run).
#[cfg(target_os = "solana")]
pub fn process_external_undelegate(
    pid: &pinocchio::pubkey::Pubkey,
    delegated: &AccountInfo,
    buffer: &AccountInfo,
    payer: &AccountInfo,
    system_program: &AccountInfo,
    seeds: &[&[u8]],
) -> ProgramResult {
    use crate::cpi::create_pda_account;
    use pinocchio::instruction::Seed;
    use pinocchio::pubkey::find_program_address;
    use pinocchio::sysvars::{rent::Rent, Sysvar};
    if !buffer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !buffer.is_owned_by(&DELEGATION_PROGRAM_ID) {
        return Err(ProgramError::IllegalOwner);
    }

    let (derived, bump) = find_program_address(seeds, pid);
    if delegated.key() != &derived {
        return Err(ProgramError::InvalidArgument);
    }

    let space = buffer.data_len();
    let lamports = Rent::get()?.minimum_balance(space);
    let bump_arr = [bump];

    // Re-open the PDA program-owned, signed by its own seeds + canonical bump.
    // Only the 2-seed (book) and 3-seed (market) PDAs are delegated, so handle
    // those explicitly (avoids a Copy-bound repeat-init of `Seed`).
    match seeds.len() {
        2 => {
            let s = [Seed::from(seeds[0]), Seed::from(seeds[1]), Seed::from(&bump_arr[..])];
            let signer = [Signer::from(&s[..])];
            create_pda_account(payer, delegated, system_program, lamports, space as u64, pid, &signer)?;
        }
        3 => {
            let s = [
                Seed::from(seeds[0]),
                Seed::from(seeds[1]),
                Seed::from(seeds[2]),
                Seed::from(&bump_arr[..]),
            ];
            let signer = [Signer::from(&s[..])];
            create_pda_account(payer, delegated, system_program, lamports, space as u64, pid, &signer)?;
        }
        _ => return Err(ProgramError::InvalidArgument),
    }

    // Copy committed state back (sizes match by construction; guard anyway).
    let src = buffer.try_borrow_data()?;
    let mut dst = delegated.try_borrow_mut_data()?;
    if dst.len() != src.len() {
        return Err(ProgramError::InvalidAccountData);
    }
    dst.copy_from_slice(&src);
    Ok(())
}
