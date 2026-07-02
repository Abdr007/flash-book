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
    system_instruction,
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

/// Borsh-serialized argument struct for the Delegate ix. Layout matches
/// `magicblock-delegation-program-api::args::DelegateArgs` byte-for-byte.
#[derive(Default, Debug, Clone, AnchorSerialize, AnchorDeserialize)]
pub struct DelegateArgs {
    /// Frequency at which the validator commits the account state if the
    /// owning program doesn't trigger commits explicitly.
    pub commit_frequency_ms: u32,
    /// Canonical PDA seeds (WITHOUT the bump) — the delegation program
    /// re-derives via find_program_address. The bump travels only in the
    /// invoke_signed signer seeds, not here (WAVE 24i).
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

    // WAVE 24i — the upgraded delegation program uses a "fast" delegate path
    // (`split_at(8)` discriminator; byte[0]=Delegate=0) that requires the CALLER
    // to stage the account into the buffer and hand its ownership to the
    // delegation program BEFORE the CPI (the old DLP did this internally). This
    // mirrors `ephemeral_rollups_sdk::cpi::delegate_account` byte-for-byte. The
    // DLP copies the buffer back into the (zeroed) account during its CPI, so the
    // round-trip is lossless. `delegated_seeds` MUST include the bump (for PDA
    // signing); `args.seeds` MUST NOT (the DLP re-derives via find_program_address).
    let data_len = accounts.delegated_account.data_len();
    let (_, buffer_bump) =
        delegate_buffer_pda(accounts.delegated_account.key, accounts.owner_program.key);
    let buffer_bump_arr = [buffer_bump];
    let buffer_signer: &[&[u8]] = &[
        DELEGATE_BUFFER_TAG,
        accounts.delegated_account.key.as_ref(),
        &buffer_bump_arr,
    ];

    // 1. Create the buffer PDA (owned by owner_program), sized to the account.
    create_pda(
        &accounts.payer,
        &accounts.delegate_buffer,
        &accounts.system_program,
        accounts.owner_program.key,
        data_len,
        &[buffer_signer],
    )?;
    // 2. Stage the account's data into the buffer.
    {
        let src = accounts.delegated_account.try_borrow_data()?;
        let mut dst = accounts.delegate_buffer.try_borrow_mut_data()?;
        require!(dst.len() == src.len(), crate::FlashBookError::OutOfRange);
        dst.copy_from_slice(&src);
    }
    // 3. Zero the account (required before its ownership can be handed off).
    {
        let mut d = accounts.delegated_account.try_borrow_mut_data()?;
        for b in d.iter_mut() {
            *b = 0;
        }
    }
    // 4. Hand ownership to the delegation program: a program may assign its own
    //    zeroed account to System, then System re-assigns it under PDA signature.
    if accounts.delegated_account.owner != accounts.system_program.key {
        accounts
            .delegated_account
            .assign(accounts.system_program.key);
    }
    if accounts.delegated_account.owner != accounts.delegation_program.key {
        invoke_signed(
            &system_instruction::assign(accounts.delegated_account.key, &DELEGATION_PROGRAM_ID),
            &[
                accounts.delegated_account.clone(),
                accounts.system_program.clone(),
            ],
            &[delegated_seeds],
        )?;
    }

    // 5. CPI the delegate (8-byte discriminator + DelegateArgs).
    let mut data = Vec::with_capacity(64);
    data.extend_from_slice(&[DELEGATE_DISCRIMINATOR, 0, 0, 0, 0, 0, 0, 0]);
    args.serialize(&mut data)?;
    let ix = Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*accounts.payer.key, true),
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
            accounts.payer.clone(),
            accounts.delegated_account.clone(),
            accounts.owner_program.clone(),
            accounts.delegate_buffer.clone(),
            accounts.delegation_record.clone(),
            accounts.delegation_metadata.clone(),
            accounts.system_program.clone(),
            accounts.delegation_program.clone(),
        ],
        &[delegated_seeds],
    )?;

    // 6. Close the now-consumed buffer, refunding rent to the payer. Draining
    //    its lamports to zero is sufficient — the runtime reclaims the account
    //    at the end of the instruction.
    let buf_lamports = accounts.delegate_buffer.lamports();
    **accounts.payer.try_borrow_mut_lamports()? = accounts
        .payer
        .lamports()
        .checked_add(buf_lamports)
        .ok_or(crate::FlashBookError::ArithmeticOverflow)?;
    **accounts.delegate_buffer.try_borrow_mut_lamports()? = 0;
    Ok(())
}

// NOTE: the program-initiated `cpi_undelegate` (+ its `UndelegateAccounts` /
// `UNDELEGATE_DISCRIMINATOR`) was removed in the 2026-06 dead-code cleanup. Real
// undelegation is driven by the MagicBlock delegation program calling back into
// `process_undelegation` → `process_external_undelegate` (the EXTERNAL_UNDELEGATE
// path below); the program never issues an Undelegate CPI itself.

/// Pure decision for the permissionless force-undelegate timeout gate
/// (`force_undelegate_market_book`). Returns true iff the ER has been silent for
/// STRICTLY MORE than `timeout_slots`. The liveness baseline is the more recent
/// of the last committed fill (`last_mark_update_slot`) and the delegation slot
/// (`book_delegated_at_slot`); the latter closes the "delegate then never fill"
/// trap. A 0 baseline (book not delegated via the upgraded path) is never
/// escapable. Extracted pure so the "never fires while the ER is live" property
/// is host-tested and Kani-proven, and the handler calls the proven function.
/// F3 (audit 2026-06): two-tier so a healthy-but-QUIET market is not griefed off
/// the ER, while a CENSORING sequencer still cannot trap funds (preserving F1).
///   • FAST path — the ER shows NO liveness of any kind (no fill, no heartbeat,
///     no recent delegation) for `stall_timeout_slots`. Heartbeat-aware, so a
///     live ER that simply has no trades keeps this shut.
///   • CENSORSHIP backstop — SETTLEMENT (a committed fill) has not advanced for
///     the much longer `censorship_timeout_slots`, IGNORING the heartbeat (an
///     alive-but-censoring sequencer heartbeats but settles nothing).
/// Either opens the escape. A zero baseline (book never delegated via the
/// upgraded path and never stamped) is never escapable.
#[inline]
pub fn force_undelegate_allowed(
    current_slot: u64,
    last_mark_update_slot: u64,
    last_heartbeat_slot: u64,
    book_delegated_at_slot: u64,
    stall_timeout_slots: u64,
    censorship_timeout_slots: u64,
) -> bool {
    // FAST: any liveness signal (fill, heartbeat, or the delegation baseline).
    let er_baseline = max3(last_mark_update_slot, last_heartbeat_slot, book_delegated_at_slot);
    let er_stalled =
        er_baseline != 0 && current_slot.saturating_sub(er_baseline) > stall_timeout_slots;

    // BACKSTOP: settlement liveness only (heartbeat deliberately excluded).
    let settle_baseline = if last_mark_update_slot > book_delegated_at_slot {
        last_mark_update_slot
    } else {
        book_delegated_at_slot
    };
    let censored = settle_baseline != 0
        && current_slot.saturating_sub(settle_baseline) > censorship_timeout_slots;

    er_stalled || censored
}

#[inline]
fn max3(a: u64, b: u64, c: u64) -> u64 {
    let ab = if a > b { a } else { b };
    if ab > c {
        ab
    } else {
        c
    }
}

/// FV: the force-undelegate gate NEVER fires while the ER is live — i.e. it can
/// only return true when the most recent liveness signal is older than the full
/// timeout. Guarantees a censoring/stalled ER is a precondition for the escape,
/// so it cannot be used to grief a healthy venue.
#[cfg(kani)]
mod force_undelegate_kani_proofs {
    use super::force_undelegate_allowed;

    /// SOUNDNESS: the escape only opens when a real liveness baseline is stale —
    /// either the ER shows no signal past the stall timeout, OR settlement has
    /// not advanced past the (longer) censorship timeout.
    #[kani::proof]
    fn only_fires_when_a_baseline_is_stale() {
        let current: u64 = kani::any();
        let last_fill: u64 = kani::any();
        let heartbeat: u64 = kani::any();
        let delegated: u64 = kani::any();
        let stall: u64 = kani::any();
        let censor: u64 = kani::any();
        if force_undelegate_allowed(current, last_fill, heartbeat, delegated, stall, censor) {
            let er_baseline = core::cmp::max(last_fill, core::cmp::max(heartbeat, delegated));
            let settle_baseline = core::cmp::max(last_fill, delegated);
            let er_stalled = er_baseline != 0 && current.saturating_sub(er_baseline) > stall;
            let censored =
                settle_baseline != 0 && current.saturating_sub(settle_baseline) > censor;
            assert!(er_stalled || censored);
        }
    }

    /// F3 ANTI-GRIEF: a market with a FRESH ER signal (recent fill or heartbeat
    /// within the stall window) AND recent settlement (within the censorship
    /// window) can NEVER be force-undelegated — so a healthy/heartbeating market
    /// is not griefed off the ER.
    #[kani::proof]
    fn fresh_heartbeat_and_settlement_cannot_be_undelegated() {
        let current: u64 = kani::any();
        let last_fill: u64 = kani::any();
        let heartbeat: u64 = kani::any();
        let delegated: u64 = kani::any();
        let stall: u64 = kani::any();
        let censor: u64 = kani::any();
        // ER alive within the stall window (heartbeat fresh) ...
        kani::assume(heartbeat <= current && current - heartbeat <= stall);
        // ... and settlement within the censorship window (recent fill).
        kani::assume(last_fill <= current && current - last_fill <= censor);
        assert!(!force_undelegate_allowed(
            current, last_fill, heartbeat, delegated, stall, censor
        ));
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

    // HIGH-4 DIAGNOSTIC (temporary, non-enforcing): capture the REAL undelegation
    // buffer address the DLP passes, so the binding seed/program can be derived
    // offline. Removed once the exact derivation is confirmed against the live ER.
    msg!("HIGH4DIAG buf={} deleg={}", buffer.key, delegated.key);

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

    // Censorship backstop is far away in these fast-path tests (heartbeat=0).
    const CENSOR: u64 = 9_000;

    #[test]
    fn force_undelegate_blocked_without_baseline() {
        // No liveness baseline (never delegated via upgraded path) ⇒ never escapable.
        assert!(!force_undelegate_allowed(1_000_000, 0, 0, 0, 750, CENSOR));
    }

    #[test]
    fn force_undelegate_blocked_while_live() {
        // Last fill 1 slot ago, timeout 750 ⇒ ER is live ⇒ blocked.
        assert!(!force_undelegate_allowed(1000, 999, 0, 500, 750, CENSOR));
        // Exactly at the timeout boundary is NOT enough (strictly greater).
        assert!(!force_undelegate_allowed(1750, 1000, 0, 0, 750, CENSOR));
    }

    #[test]
    fn force_undelegate_allowed_after_timeout() {
        // 751 slots since last fill AND no heartbeat ⇒ ER dark ⇒ escape opens.
        assert!(force_undelegate_allowed(1751, 1000, 0, 0, 750, CENSOR));
        // Sequencer delegated then never filled/heartbeat: baseline = delegation slot.
        assert!(force_undelegate_allowed(2000, 0, 0, 1000, 750, CENSOR));
        // The MORE RECENT signal wins (a fresh fill keeps it live even if old delegation).
        assert!(!force_undelegate_allowed(2000, 1900, 0, 100, 750, CENSOR));
    }

    #[test]
    fn f2_f3_fresh_heartbeat_blocks_fast_escape_on_quiet_market() {
        // F3: a QUIET but healthy market — last fill 5_000 slots ago (≫ stall 750,
        // but within the 9_000 censorship window so the backstop is NOT in play),
        // and the ER heartbeats every ~100 slots. WITHOUT the heartbeat the fast
        // path would fire (5_000 > 750); WITH it the escape must stay SHUT (no grief).
        // current 100_000, last_fill 95_000, heartbeat 99_900, delegated 1_000.
        assert!(!force_undelegate_allowed(100_000, 95_000, 99_900, 1_000, 750, CENSOR));
        // Same market, ER now ALSO stops heartbeating for > 750 slots ⇒ dark ⇒ escape.
        assert!(force_undelegate_allowed(100_000, 95_000, 99_000, 1_000, 750, CENSOR));
    }

    #[test]
    fn f3_censorship_backstop_fires_despite_fresh_heartbeat() {
        // F1 preserved: an alive-but-CENSORING sequencer heartbeats every slot but
        // settles NOTHING. The fast path stays shut (heartbeat fresh), but the
        // censorship backstop opens once settlement is older than CENSOR.
        // current 1_000_000, last_fill 990_000 (10k ago > CENSOR 9_000),
        // heartbeat 999_999 (1 slot ago), delegated 990_000.
        assert!(force_undelegate_allowed(1_000_000, 990_000, 999_999, 990_000, 750, CENSOR));
        // If the fill is within the censorship window, NOT escapable (still trading).
        assert!(!force_undelegate_allowed(1_000_000, 995_000, 999_999, 990_000, 750, CENSOR));
    }

    #[test]
    fn f1_stamp_baseline_closes_the_pre_upgrade_trap() {
        // F1: a market delegated BEFORE the upgrade has both signals at 0. Its ER
        // goes dark with no committed fill → baseline 0 → trapped forever.
        let timeout = 750;
        assert!(
            !force_undelegate_allowed(10_000_000, 0, 0, 0, timeout, CENSOR),
            "pre-upgrade market with no baseline must be trapped (the F1 bug)"
        );
        // stamp_book_liveness_baseline sets book_delegated_at_slot = current slot.
        let stamp_slot = 10_000_000;
        // Immediately after stamping, the ER has NOT yet been silent past the
        // timeout, so the escape stays closed (cannot be used to grief).
        assert!(!force_undelegate_allowed(stamp_slot, 0, 0, stamp_slot, timeout, CENSOR));
        assert!(!force_undelegate_allowed(stamp_slot + timeout, 0, 0, stamp_slot, timeout, CENSOR));
        // A genuinely live ER that posts a fill after the stamp pushes the
        // baseline forward via last_mark_update_slot → still blocked.
        assert!(!force_undelegate_allowed(
            stamp_slot + timeout + 1,
            stamp_slot + 5,
            0,
            stamp_slot,
            timeout,
            CENSOR
        ));
        // After a FULL timeout of continued silence post-stamp (no fill, no
        // heartbeat), the trapped trader can finally escape — the trap is closed.
        assert!(force_undelegate_allowed(stamp_slot + timeout + 1, 0, 0, stamp_slot, timeout, CENSOR));
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
