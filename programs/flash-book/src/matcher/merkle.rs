//! Merkle inclusion — M-14 endgame bridge (Phase 2.5).
//!
//! A committed batch (`commit_batch`) records a `fills_merkle_root`. To settle an
//! individual fill against a committee-attested batch, settlement must prove that
//! fill's leaf is IN that root. This module is the pure, generic fold that does
//! it: given a leaf and an inclusion proof (sibling hash + side per level), it
//! reconstructs the root. It is generic over the two-child `combine` hash so the
//! FOLD LOGIC is Kani-provable independent of keccak (which lives handler-side);
//! `lib.rs` supplies the keccak combiner. Not yet wired into settlement — that is
//! the final phase, done in lockstep with the off-chain engine. See
//! `docs/DECENTRALIZED_SEQUENCER.md`.

/// One level of an inclusion proof: the sibling hash and whether the sibling is
/// the LEFT child (so the running node is the right child) at this level.
pub type ProofNode = ([u8; 32], bool);

/// Reconstruct the merkle root from `leaf` and `proof`. `combine(l, r)` hashes an
/// ordered pair of children. An empty proof means `leaf` IS the root (single-leaf
/// tree). Order is preserved exactly (`sib_is_left` decides which side the sibling
/// sits) so the root is sensitive to position — a proof cannot be replayed at the
/// wrong index.
#[inline]
pub fn fold_proof<F>(leaf: [u8; 32], proof: &[ProofNode], combine: F) -> [u8; 32]
where
    F: Fn(&[u8; 32], &[u8; 32]) -> [u8; 32],
{
    let mut node = leaf;
    for (sibling, sib_is_left) in proof {
        node = if *sib_is_left {
            combine(sibling, &node)
        } else {
            combine(&node, sibling)
        };
    }
    node
}

/// True iff `leaf` folds up to `root` under `proof`.
#[inline]
pub fn verify_inclusion<F>(leaf: [u8; 32], proof: &[ProofNode], root: [u8; 32], combine: F) -> bool
where
    F: Fn(&[u8; 32], &[u8; 32]) -> [u8; 32],
{
    fold_proof(leaf, proof, combine) == root
}

#[cfg(kani)]
mod proofs {
    use super::*;

    // A deterministic mock combiner. Order-sensitivity of the ROOT is a property
    // of the real combiner (keccak collision-resistance), NOT of the fold — so
    // these proofs verify only the fold's STRUCTURAL guarantees, which hold for
    // any `combine` and don't depend on the mock being collision-free.
    fn mock(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
        let mut o = [0u8; 32];
        for i in 0..32 {
            o[i] = l[i]
                .wrapping_mul(3)
                .wrapping_add(r[i].wrapping_mul(7))
                .wrapping_add(i as u8);
        }
        o
    }

    /// Empty proof ⇒ the leaf is the root (single-leaf tree identity). ∀ leaf.
    #[kani::proof]
    fn empty_proof_is_leaf() {
        let leaf: [u8; 32] = kani::any();
        assert!(verify_inclusion(leaf, &[], leaf, mock));
    }

    /// A single proof level applies the sibling's SIDE correctly: sibling-on-right
    /// folds to `combine(node, sib)`, sibling-on-left to `combine(sib, node)`. This
    /// is the structural core that makes an inclusion proof position-bound — the
    /// running node can't silently be placed on the wrong side. ∀ (leaf, sib),
    /// combiner-agnostic (holds for keccak).
    #[kani::proof]
    fn single_level_applies_side_correctly() {
        let leaf: [u8; 32] = kani::any();
        let sib: [u8; 32] = kani::any();
        assert_eq!(fold_proof(leaf, &[(sib, false)], mock), mock(&leaf, &sib));
        assert_eq!(fold_proof(leaf, &[(sib, true)], mock), mock(&sib, &leaf));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real keccak combiner (host build only) for end-to-end tree tests.
    fn kc(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
        solana_keccak_hasher::hashv(&[l, r]).0
    }
    fn leaf(b: u8) -> [u8; 32] {
        solana_keccak_hasher::hashv(&[&[b]]).0
    }

    #[test]
    fn single_leaf_root_is_leaf() {
        let l = leaf(1);
        assert!(verify_inclusion(l, &[], l, kc));
        assert!(!verify_inclusion(l, &[], leaf(2), kc));
    }

    #[test]
    fn four_leaf_tree_inclusion() {
        // leaves a b c d → n0=H(a,b) n1=H(c,d) → root=H(n0,n1)
        let (a, b, c, d) = (leaf(1), leaf(2), leaf(3), leaf(4));
        let n0 = kc(&a, &b);
        let n1 = kc(&c, &d);
        let root = kc(&n0, &n1);

        // proof for `a` (leftmost): sibling b (right), then n1 (right)
        assert!(verify_inclusion(a, &[(b, false), (n1, false)], root, kc));
        // proof for `d` (rightmost): sibling c (left), then n0 (left)
        assert!(verify_inclusion(d, &[(c, true), (n0, true)], root, kc));
        // proof for `c`: sibling d (right), then n0 (left)
        assert!(verify_inclusion(c, &[(d, false), (n0, true)], root, kc));
    }

    #[test]
    fn rejects_tampering() {
        let (a, b, c, d) = (leaf(1), leaf(2), leaf(3), leaf(4));
        let n1 = kc(&c, &d);
        let root = kc(&kc(&a, &b), &n1);
        // wrong leaf
        assert!(!verify_inclusion(
            leaf(9),
            &[(b, false), (n1, false)],
            root,
            kc
        ));
        // wrong sibling
        assert!(!verify_inclusion(
            a,
            &[(leaf(9), false), (n1, false)],
            root,
            kc
        ));
        // wrong side (claim b is on the left) → different root
        assert!(!verify_inclusion(a, &[(b, true), (n1, false)], root, kc));
        // non-member with a fabricated proof cannot hit the root
        assert!(!verify_inclusion(leaf(7), &[(leaf(8), false)], root, kc));
    }
}
