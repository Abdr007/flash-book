//! Sequencer committee — M-14 decentralization primitive (Phase 1 scaffold).
//!
//! Pure membership + BFT-quorum logic for the on-chain `SequencerCommittee`
//! (`state_v3`). Phase 1 lands the data structure + governance + this verified
//! logic; generalizing settlement authorization to the committee (the
//! threshold-attested `settle_batch` path) is a later phase — see
//! `docs/DECENTRALIZED_SEQUENCER.md`. Kept pure so the quorum math is Kani-proven
//! independent of account plumbing, and so it can't perturb the settlement path
//! while it is only scaffolding.

use anchor_lang::prelude::Pubkey;

/// A BFT quorum requires `threshold > 2N/3` (equivalently `3·threshold > 2·N`):
/// any two quorums then intersect in ≥1 validator (safety), and `N ≥ 3f+1`
/// tolerates `f` Byzantine. Returns true iff `(validator_count, threshold)` is a
/// valid BFT committee configuration. `N = 1, threshold = 1` (the Phase-1
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

#[cfg(kani)]
mod proofs {
    use super::is_valid_bft_config;

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
