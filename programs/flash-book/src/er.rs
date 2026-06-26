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
    program::{invoke, invoke_signed},
    pubkey, system_instruction,
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

/// Pure decision for the permissionless force-undelegate timeout gate
/// (`force_undelegate_market_book`). Returns true iff the ER has been silent for
/// STRICTLY MORE than `timeout_slots`. The liveness baseline is the more recent
/// of the last committed fill (`last_mark_update_slot`) and the delegation slot
/// (`book_delegated_at_slot`); the latter closes the "delegate then never fill"
/// trap. A 0 baseline (book not delegated via the upgraded path) is never
/// escapable. Extracted pure so the "never fires while the ER is live" property
/// is host-tested and Kani-proven, and the handler calls the proven function.
#[inline]
pub fn force_undelegate_allowed(
    current_slot: u64,
    last_mark_update_slot: u64,
    book_delegated_at_slot: u64,
    timeout_slots: u64,
) -> bool {
    let baseline = if last_mark_update_slot > book_delegated_at_slot {
        last_mark_update_slot
    } else {
        book_delegated_at_slot
    };
    if baseline == 0 {
        return false;
    }
    current_slot.saturating_sub(baseline) > timeout_slots
}

/// FV: the force-undelegate gate NEVER fires while the ER is live — i.e. it can
/// only return true when the most recent liveness signal is older than the full
/// timeout. Guarantees a censoring/stalled ER is a precondition for the escape,
/// so it cannot be used to grief a healthy venue.
#[cfg(kani)]
mod force_undelegate_kani_proofs {
    use super::force_undelegate_allowed;

    #[kani::proof]
    fn never_fires_while_live() {
        let current: u64 = kani::any();
        let last_fill: u64 = kani::any();
        let delegated: u64 = kani::any();
        let timeout: u64 = kani::any();
        if force_undelegate_allowed(current, last_fill, delegated, timeout) {
            // Then BOTH liveness signals are older than the full timeout window,
            // and at least one real baseline exists.
            let baseline = core::cmp::max(last_fill, delegated);
            assert!(baseline > 0);
            assert!(current > baseline);
            assert!(current - baseline > timeout);
        }
    }
}

// ─── MagicBlock Magic program (ER-side commit) + undelegation callback ───
//
// The pieces below complete the settlement loop the bare Delegate/Undelegate
// CPIs above cannot do on their own (audit finding ER-2 / C-2):
//
//   * `cpi_commit` — an ON-THE-ER Magic Action (CPI to the Magic program) that
//     either snapshots delegated state back to L1 (`ScheduleCommit`) or
//     snapshots + queues undelegation (`ScheduleCommitAndUndelegate`).
//   * `process_external_undelegate` — the BASE-LAYER callback the delegation
//     program invokes (prefixed with EXTERNAL_UNDELEGATE_DISCRIMINATOR) to
//     re-open the PDA under this program and copy the committed buffer back.
//
// Wire format hand-rolled (we can't depend on `ephemeral-rollups-sdk`, see the
// module header) but byte-verified against the SDK source + its bincode tests:
//   ScheduleCommit               -> u32 LE enum tag [1,0,0,0]
//   ScheduleCommitAndUndelegate  -> u32 LE enum tag [2,0,0,0]

/// MagicBlock Magic (validator) program — runs ON the ER. `declare_id!` in
/// `magicblock-magic-program-api`.
pub const MAGIC_PROGRAM_ID: Pubkey = pubkey!("Magic11111111111111111111111111111111111111");
/// MagicBlock Magic context account (writable; collects scheduled commits).
pub const MAGIC_CONTEXT_ID: Pubkey = pubkey!("MagicContext1111111111111111111111111111111");
/// 8-byte prefix the delegation program uses when it CPIs BACK into this
/// program to finalize an undelegation. Dispatched in the program `fallback`.
pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR: [u8; 8] = [196, 28, 41, 206, 48, 37, 51, 167];

/// `MagicBlockInstruction::ScheduleCommit` (bincode u32 LE enum tag).
const SCHEDULE_COMMIT: [u8; 4] = [1, 0, 0, 0];
/// `MagicBlockInstruction::ScheduleCommitAndUndelegate`.
const SCHEDULE_COMMIT_AND_UNDELEGATE: [u8; 4] = [2, 0, 0, 0];

/// ON-THE-ER Magic Action: schedule a commit of `committed` accounts back to
/// the base layer. If `allow_undelegation`, also queues undelegation (after
/// which the delegation program will CPI `process_external_undelegate` on base).
///
/// Account order MUST be `[payer(signer,w), magic_context(w), ...committed]`
/// — the Magic program appends committed accounts after the context. Plain
/// `invoke` (the payer signs; no PDA seeds required for the commit itself).
pub fn cpi_commit<'info>(
    payer: &AccountInfo<'info>,
    magic_context: &AccountInfo<'info>,
    magic_program: &AccountInfo<'info>,
    committed: &[AccountInfo<'info>],
    allow_undelegation: bool,
) -> Result<()> {
    require_keys_eq!(
        *magic_program.key,
        MAGIC_PROGRAM_ID,
        crate::FlashBookError::Unauthorized
    );
    require_keys_eq!(
        *magic_context.key,
        MAGIC_CONTEXT_ID,
        crate::FlashBookError::Unauthorized
    );

    let data: &[u8] = if allow_undelegation {
        &SCHEDULE_COMMIT_AND_UNDELEGATE
    } else {
        &SCHEDULE_COMMIT
    };

    let mut metas = Vec::with_capacity(2 + committed.len());
    metas.push(AccountMeta::new(*payer.key, true));
    metas.push(AccountMeta::new(*magic_context.key, false)); // writable, not signer
    for a in committed {
        metas.push(AccountMeta {
            pubkey: *a.key,
            is_signer: a.is_signer,
            is_writable: a.is_writable,
        });
    }

    let ix = Instruction {
        program_id: MAGIC_PROGRAM_ID,
        accounts: metas,
        data: data.to_vec(),
    };

    let mut infos = Vec::with_capacity(3 + committed.len());
    infos.push(payer.clone());
    infos.push(magic_context.clone());
    infos.extend(committed.iter().cloned());
    infos.push(magic_program.clone());

    invoke(&ix, &infos)?;
    Ok(())
}

/// Re-create / re-own a PDA under `owner` with `space` bytes, signed by its
/// seeds. Mirrors the SDK `create_pda`: fresh account -> `create_account`;
/// pre-existing (lamport-carrying) account -> top up rent + `allocate` +
/// `assign`. 2.1-clean (no SDK dependency).
fn create_pda<'info>(
    payer: &AccountInfo<'info>,
    pda: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    owner: &Pubkey,
    space: usize,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let rent = Rent::get()?;
    let required = rent.minimum_balance(space);
    let current = pda.lamports();

    if current == 0 {
        let ix =
            system_instruction::create_account(payer.key, pda.key, required, space as u64, owner);
        invoke_signed(
            &ix,
            &[payer.clone(), pda.clone(), system_program.clone()],
            signer_seeds,
        )?;
    } else {
        if current < required {
            let ix = system_instruction::transfer(payer.key, pda.key, required - current);
            invoke(&ix, &[payer.clone(), pda.clone(), system_program.clone()])?;
        }
        let alloc = system_instruction::allocate(pda.key, space as u64);
        invoke_signed(&alloc, &[pda.clone(), system_program.clone()], signer_seeds)?;
        let assign = system_instruction::assign(pda.key, owner);
        invoke_signed(&assign, &[pda.clone(), system_program.clone()], signer_seeds)?;
    }
    Ok(())
}

/// BASE-LAYER callback the delegation program invokes (after the ER processed
/// `commit_and_undelegate`) to finalize undelegation.
///
/// IMPORTANT: `EXTERNAL_UNDELEGATE_DISCRIMINATOR` is **exactly**
/// `sha256("global:process_undelegation")[..8]` — i.e. the delegation program
/// calls back via the *normal Anchor instruction* `process_undelegation`, not a
/// raw/non-Anchor discriminator. So the wiring is an ordinary `#[program]` ix
/// (see `process_undelegation` in `lib.rs`), and Anchor deserializes the
/// `account_seeds: Vec<Vec<u8>>` arg for us — no entrypoint fallback needed.
///
/// This mirrors `ephemeral_rollups_sdk::cpi::undelegate_account`. Accounts:
/// `delegated`(w), `buffer`(signer, owned by the delegation program), `payer`(w),
/// `system_program`. The buffer (filled by the validator with the committed ER
/// state) is copied byte-for-byte into the re-opened PDA.
pub fn process_external_undelegate<'info>(
    delegated: &AccountInfo<'info>,
    buffer: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    seeds: Vec<Vec<u8>>,
) -> Result<()> {
    // Only the delegation program's signed buffer may drive this callback.
    require!(buffer.is_signer, crate::FlashBookError::Unauthorized);
    require_keys_eq!(
        *buffer.owner,
        DELEGATION_PROGRAM_ID,
        crate::FlashBookError::Unauthorized
    );

    // Re-derive the canonical bump and verify the target PDA matches the seeds.
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived, bump) = Pubkey::find_program_address(&seed_slices, &crate::ID);
    require_keys_eq!(*delegated.key, derived, crate::FlashBookError::WrongMarket);

    let bump_arr = [bump];
    let mut signer: Vec<&[u8]> = seed_slices.clone();
    signer.push(&bump_arr);

    // Re-open the PDA under THIS program, sized to the committed buffer.
    create_pda(
        payer,
        delegated,
        system_program,
        &crate::ID,
        buffer.data_len(),
        &[&signer],
    )?;

    // Copy committed state back. Sizes match by construction (PDA created with
    // buffer.data_len()); guard anyway — Solana programs must not panic.
    let src = buffer.try_borrow_data()?;
    let mut dst = delegated.try_borrow_mut_data()?;
    require!(
        dst.len() == src.len(),
        crate::FlashBookError::OutOfRange
    );
    dst.copy_from_slice(&src);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_undelegate_blocked_without_baseline() {
        // No liveness baseline (never delegated via upgraded path) ⇒ never escapable.
        assert!(!force_undelegate_allowed(1_000_000, 0, 0, 750));
    }

    #[test]
    fn force_undelegate_blocked_while_live() {
        // Last fill 1 slot ago, timeout 750 ⇒ ER is live ⇒ blocked.
        assert!(!force_undelegate_allowed(1000, 999, 500, 750));
        // Exactly at the timeout boundary is NOT enough (strictly greater).
        assert!(!force_undelegate_allowed(1750, 1000, 0, 750));
    }

    #[test]
    fn force_undelegate_allowed_after_timeout() {
        // 751 slots since last fill ⇒ escape opens.
        assert!(force_undelegate_allowed(1751, 1000, 0, 750));
        // Sequencer delegated then never filled: baseline = delegation slot.
        assert!(force_undelegate_allowed(2000, 0, 1000, 750));
        // The MORE RECENT signal wins (a fresh fill keeps it live even if old delegation).
        assert!(!force_undelegate_allowed(2000, 1900, 100, 750));
    }

    #[test]
    fn f1_stamp_baseline_closes_the_pre_upgrade_trap() {
        // F1: a market delegated BEFORE the upgrade has both signals at 0. Its ER
        // goes dark with no committed fill → baseline 0 → trapped forever.
        let timeout = 750;
        assert!(
            !force_undelegate_allowed(10_000_000, 0, 0, timeout),
            "pre-upgrade market with no baseline must be trapped (the F1 bug)"
        );
        // stamp_book_liveness_baseline sets book_delegated_at_slot = current slot.
        let stamp_slot = 10_000_000;
        // Immediately after stamping, the ER has NOT yet been silent past the
        // timeout, so the escape stays closed (cannot be used to grief).
        assert!(!force_undelegate_allowed(stamp_slot, 0, stamp_slot, timeout));
        assert!(!force_undelegate_allowed(stamp_slot + timeout, 0, stamp_slot, timeout));
        // A genuinely live ER that posts a fill after the stamp pushes the
        // baseline forward via last_mark_update_slot → still blocked.
        assert!(!force_undelegate_allowed(
            stamp_slot + timeout + 1,
            stamp_slot + 5,
            stamp_slot,
            timeout
        ));
        // After a FULL timeout of continued silence post-stamp, the trapped
        // trader can finally escape — the trap is closed.
        assert!(force_undelegate_allowed(stamp_slot + timeout + 1, 0, stamp_slot, timeout));
    }

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

    #[test]
    fn magic_program_and_context_ids_are_canonical() {
        assert_eq!(
            MAGIC_PROGRAM_ID.to_string(),
            "Magic11111111111111111111111111111111111111"
        );
        assert_eq!(
            MAGIC_CONTEXT_ID.to_string(),
            "MagicContext1111111111111111111111111111111"
        );
    }

    #[test]
    fn commit_enum_tags_match_magicblock_abi() {
        // bincode(MagicBlockInstruction) — u32 LE enum discriminant.
        assert_eq!(SCHEDULE_COMMIT, [1, 0, 0, 0]);
        assert_eq!(SCHEDULE_COMMIT_AND_UNDELEGATE, [2, 0, 0, 0]);
    }

    #[test]
    fn external_undelegate_discriminator_is_canonical() {
        assert_eq!(
            EXTERNAL_UNDELEGATE_DISCRIMINATOR,
            [196, 28, 41, 206, 48, 37, 51, 167]
        );
    }

    #[test]
    fn undelegate_callback_seeds_borsh_roundtrip() {
        // The delegation program sends EXTERNAL_UNDELEGATE_DISCRIMINATOR ++
        // borsh(Vec<Vec<u8>>); process_external_undelegate decodes the tail.
        let seeds: Vec<Vec<u8>> = vec![b"market_book".to_vec(), Pubkey::new_unique().to_bytes().to_vec()];
        let mut buf = Vec::new();
        seeds.serialize(&mut buf).unwrap();
        let decoded = Vec::<Vec<u8>>::try_from_slice(&buf).unwrap();
        assert_eq!(decoded, seeds);
    }
}
