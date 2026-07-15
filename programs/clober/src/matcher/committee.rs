//! Sequencer committee — decentralized-sequencer primitive.
//!
//! Pure membership + BFT-quorum logic for the on-chain `SequencerCommittee`
//! (`extended_state`). The committee attests batch state transitions
//! (`commit_batch`); fill settlement authorization (`apply_fill`) remains
//! bound to the market's single settlement signer. Kept pure so the quorum
//! math is Kani-proven independent of account plumbing and cannot perturb
//! the settlement path.

use anchor_lang::prelude::Pubkey;

/// A BFT quorum requires `threshold > 2N/3` (equivalently `3·threshold > 2·N`):
/// any two quorums then intersect in ≥1 validator (safety), and `N ≥ 3f+1`
/// tolerates `f` Byzantine. Returns true iff `(validator_count, threshold)` is a
/// valid BFT committee configuration. `N = 1, threshold = 1` (the
/// backward-compatible single-sequencer case) satisfies it (`3 > 2`).
#[inline]
pub fn is_valid_bft_config(validator_count: u8, threshold: u8) -> bool {
    let n = validator_count as u32;
    let t = threshold as u32;
    n >= 1 && t >= 1 && t <= n && 3 * t > 2 * n
}

/// True iff `key` is one of the first `validator_count` entries of `validators`.
#[inline]
pub fn is_committee_member(validators: &[Pubkey], validator_count: u8, key: &Pubkey) -> bool {
    let n = (validator_count as usize).min(validators.len());
    validators[..n].iter().any(|v| v == key)
}

/// True iff the `validators[..count]` prefix has no duplicate and no
/// all-zero (default) key — each quorum slot must be a distinct, real validator,
/// so one key can't fill multiple slots toward the threshold.
pub fn validators_valid_set(validators: &[Pubkey], validator_count: u8) -> bool {
    let n = (validator_count as usize).min(validators.len());
    let zero = Pubkey::default();
    for i in 0..n {
        if validators[i] == zero {
            return false;
        }
        for j in (i + 1)..n {
            if validators[i] == validators[j] {
                return false;
            }
        }
    }
    true
}

/// PHASE 2 quorum check. Given the committee's validator set and the list of
/// `attestors` that each PRECOMPILE-verified a signature over the batch message,
/// returns true iff the attestation is a valid quorum: every attestor is a
/// distinct committee member and there are `>= threshold` of them. The native
/// Ed25519 precompile already checked the signature MATH (per attestor); this
/// enforces the SET properties that make the quorum meaningful — no duplicate
/// key filling multiple slots, no non-member, and the threshold is met.
pub fn attestation_meets_threshold(
    validators: &[Pubkey],
    validator_count: u8,
    threshold: u8,
    attestors: &[Pubkey],
) -> bool {
    if attestors.len() < threshold as usize || threshold == 0 {
        return false;
    }
    // Each attestor must be a distinct committee member.
    for (i, a) in attestors.iter().enumerate() {
        if !is_committee_member(validators, validator_count, a) {
            return false;
        }
        for b in &attestors[i + 1..] {
            if a == b {
                return false; // duplicate attestor
            }
        }
    }
    true
}

/// True iff committee slot `slot` is jailed (its bit is set in `jailed_mask`).
#[inline]
pub fn is_jailed(jailed_mask: u64, slot: u8) -> bool {
    (slot as u32) < 64 && (jailed_mask & (1u64 << slot)) != 0
}

/// Jail `slot` (idempotent set of its bit). Slots ≥ 64 are ignored.
#[inline]
pub fn jail_slot(jailed_mask: u64, slot: u8) -> u64 {
    if (slot as u32) < 64 {
        jailed_mask | (1u64 << slot)
    } else {
        jailed_mask
    }
}

/// PHASE 2.6 equivocation predicate. Two attestations by the SAME validator are a
/// slashable equivocation iff they cover the SAME consensus height (`epoch`,
/// `batch_seq`) but commit to DIFFERENT content (distinct signed digest). A BFT
/// validator must sign at most one value per height; two distinct values at one
/// height is the canonical, provable fault (and, by quorum intersection, what a
/// safety violation requires).
#[inline]
pub fn is_equivocation(
    epoch_a: u64,
    seq_a: u64,
    digest_a: &[u8; 32],
    epoch_b: u64,
    seq_b: u64,
    digest_b: &[u8; 32],
) -> bool {
    epoch_a == epoch_b && seq_a == seq_b && digest_a != digest_b
}

#[cfg(kani)]
mod proofs {
    use super::{is_equivocation, is_jailed, is_valid_bft_config, jail_slot};

    /// Jailing is a monotone idempotent set: after `jail_slot`, the slot reads
    /// jailed, and re-jailing changes nothing. ∀ (mask, slot < 64).
    #[kani::proof]
    fn jail_then_is_jailed() {
        let mask: u64 = kani::any();
        let slot: u8 = kani::any();
        kani::assume((slot as u32) < 64);
        let updated_mask = jail_slot(mask, slot);
        assert!(is_jailed(updated_mask, slot));
        assert_eq!(jail_slot(updated_mask, slot), updated_mask); // idempotent
    }

    /// Equivocation is exactly "same height, different digest" — and is symmetric
    /// in the two attestations. ∀ (epoch, seq, digests).
    #[kani::proof]
    fn equivocation_iff_same_height_diff_digest() {
        let e: u64 = kani::any();
        let s: u64 = kani::any();
        let da: [u8; 32] = kani::any();
        let db: [u8; 32] = kani::any();
        assert_eq!(is_equivocation(e, s, &da, e, s, &db), da != db);
        // symmetric
        assert_eq!(
            is_equivocation(e, s, &da, e, s, &db),
            is_equivocation(e, s, &db, e, s, &da)
        );
        // different height is never equivocation, whatever the digests
        assert!(!is_equivocation(e, s, &da, e, s.wrapping_add(1), &db));
    }

    /// THE safety property: a valid BFT config guarantees quorum INTERSECTION —
    /// `2·threshold > N`, so two size-`threshold` quorums drawn from `N`
    /// validators always share ≥1 member (`|A|+|B|−N ≥ 2t−N > 0`). This is what
    /// makes conflicting-batch equivocation detectable/impossible under honest
    /// majority. ∀ (validator_count, threshold).
    #[kani::proof]
    fn valid_bft_config_implies_quorum_intersection() {
        let n: u8 = kani::any();
        let t: u8 = kani::any();
        if is_valid_bft_config(n, t) {
            assert!(2 * (t as u32) > n as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase1_single_sequencer_is_valid() {
        // N=1, threshold=1 — the backward-compatible single-sequencer committee.
        assert!(is_valid_bft_config(1, 1));
    }

    #[test]
    fn rejects_sub_quorum_and_degenerate() {
        assert!(!is_valid_bft_config(0, 0)); // empty
        assert!(!is_valid_bft_config(4, 0)); // threshold 0
        assert!(!is_valid_bft_config(4, 2)); // 3*2=6 !> 2*4=8 → sub-quorum
        assert!(!is_valid_bft_config(4, 5)); // threshold > N
        assert!(is_valid_bft_config(4, 3)); // 2f+1 with f=1 (3*3=9 > 8)
        assert!(is_valid_bft_config(7, 5)); // f=2 (3*5=15 > 14)
    }

    #[test]
    fn quorum_intersection_holds_for_all_valid() {
        for n in 1u8..=32 {
            for t in 1u8..=n {
                if is_valid_bft_config(n, t) {
                    assert!(2 * (t as u32) > n as u32, "n={n} t={t}");
                }
            }
        }
    }

    #[test]
    fn attestation_quorum_set_rules() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let c = Pubkey::new_unique();
        let d = Pubkey::new_unique();
        let outsider = Pubkey::new_unique();
        let vals = [a, b, c, d]; // N=4, BFT threshold = 3 (f=1)

        assert!(attestation_meets_threshold(&vals, 4, 3, &[a, b, c])); // 3 distinct members
        assert!(attestation_meets_threshold(&vals, 4, 3, &[a, b, c, d])); // 4 ≥ 3
        assert!(!attestation_meets_threshold(&vals, 4, 3, &[a, b])); // 2 < threshold
        assert!(!attestation_meets_threshold(&vals, 4, 3, &[a, b, a])); // duplicate attestor
        assert!(!attestation_meets_threshold(&vals, 4, 3, &[a, b, outsider])); // non-member
        assert!(!attestation_meets_threshold(&vals, 4, 0, &[a, b, c])); // threshold 0
                                                                        // N=1, threshold=1 — the backward-compatible single-sequencer quorum.
        assert!(attestation_meets_threshold(&[a], 1, 1, &[a]));
        assert!(!attestation_meets_threshold(&[a], 1, 1, &[b])); // not the member
    }

    #[test]
    fn jail_and_equivocation() {
        assert!(!is_jailed(0, 0));
        let m = jail_slot(0, 3);
        assert!(is_jailed(m, 3) && !is_jailed(m, 2));
        assert_eq!(jail_slot(m, 3), m); // idempotent
        let updated_mask = jail_slot(m, 0);
        assert!(is_jailed(updated_mask, 0) && is_jailed(updated_mask, 3));

        let da = [1u8; 32];
        let db = [2u8; 32];
        assert!(is_equivocation(1, 5, &da, 1, 5, &db)); // same height, diff digest
        assert!(!is_equivocation(1, 5, &da, 1, 5, &da)); // same digest → not a fault
        assert!(!is_equivocation(1, 5, &da, 1, 6, &db)); // different seq
        assert!(!is_equivocation(1, 5, &da, 2, 5, &db)); // different epoch
    }

    #[test]
    fn membership_and_valid_set() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let zero = Pubkey::default();
        let vals = [a, b, zero, zero];
        assert!(is_committee_member(&vals, 2, &a));
        assert!(is_committee_member(&vals, 2, &b));
        assert!(!is_committee_member(&vals, 2, &Pubkey::new_unique()));
        // key beyond the count is not a member
        assert!(!is_committee_member(&vals, 1, &b));
        assert!(validators_valid_set(&vals, 2));
        assert!(!validators_valid_set(&vals, 3)); // includes a zero slot
        assert!(!validators_valid_set(&[a, a, zero, zero], 2)); // duplicate
    }
}
