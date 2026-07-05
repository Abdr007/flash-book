# Decentralized sequencer — design and on-chain status

Flash Book today runs one sequencer per market: it operates the continuous
CLOB on the Ephemeral Rollup and signs fill settlement. What that single
party can and cannot do is bounded on-chain — fill **authenticity** is
enforced by the commitment ring, settlement replay/reorder by the monotone
fill sequence, and the mark is pinned to the trustless oracle band — but
fill **ordering** and **liveness** remain a single-operator trust
assumption, stated precisely in `ER_TRUST_BOUNDARY.md`.

This document describes the committee primitive that already exists
on-chain as the additive first step toward removing that assumption, and
the target architecture the primitive is designed for. It is explicitly
**not** a batch-auction design: matching stays a continuous price-time
CLOB; consensus would decide only the *input order*.

## What exists on-chain today

Three instructions, all additive and off the settlement hot path —
settlement authorization is **not** gated on any of this, so a committee
can be stood up and exercised without touching funds:

1. **`set_sequencer_committee`** (authority): creates or rotates a
   `SequencerCommittee` PDA — up to `MAX_COMMITTEE_VALIDATORS` (32)
   distinct validators and a BFT threshold validated against
   `3·threshold > 2·N`. Rotation bumps the epoch and clears jail state.
   `N = 1, threshold = 1` reproduces the single-sequencer configuration.
2. **`commit_batch`** (permissionless): records a committee-attested state
   transition on the `BatchAttestation` PDA. It verifies the epoch matches
   the active committee, `batch_seq` strictly increases (replay/reorder
   guard), `prev_state_root` chains onto the last committed root, and at
   least `threshold` **distinct**, un-jailed validators each Ed25519-signed
   the keccak digest of the canonical batch message (native precompile,
   checked by instruction introspection). No single signer authorizes a
   batch — the quorum does.
3. **`slash_equivocation`** (permissionless): jails a validator on a fraud
   proof — two precompile-verified signatures by the same validator over
   conflicting batches at the same `(epoch, batch_seq)`. A jailed
   validator's attestations stop counting toward any quorum; governance
   re-forms the committee to clear jail state.

The quorum membership, threshold-intersection, and equivocation predicates
are pure functions in `matcher/committee.rs`, Kani-proven independent of
account plumbing.

## Target architecture

```
 traders ──orders──►  Validator set (N ≥ 3f+1)
                        │ 1. shared mempool of signed orders
                        │ 2. BFT consensus on the ORDER-FLOW SEQUENCE
                        │ 3. each validator deterministically replays the
                        │    agreed sequence through the same CLOB engine
                        │    (state-machine replication → identical fills)
                        │ 4. 2f+1 validators threshold-sign the batch
                        ▼
                 L1 / ER settlement (this program)
                  verify the committee attestation, then settle via the
                  existing apply_fill math
```

Because the sequence is consensus-determined and matching is a
deterministic function of it, no single validator controls ordering or the
mark. The settlement *math* never changes — only **who is authorized to
settle** generalizes from one signer to a quorum.

## What remains (future work, in dependency order)

1. **Fill-inclusion binding**: a settlement path that requires a fill to
   prove membership in an attested batch root (an inclusion-proof fold
   consumed at settlement). Nothing on-chain consumes inclusion proofs
   today.
2. **The off-chain system**: the BFT consensus engine, the deterministic
   CLOB replica run by every validator, validator clients, and the
   staking/slashing economics. This is the engineering bulk and lives
   outside this repository.
3. **Cutover**: gate settlement on a valid committee attestation, rotate
   1-of-1 → M-of-N on devnet under the live acceptance suite, then mainnet.

Until that cutover, the honest description of the deployed system is the
single-sequencer trust model with on-chain authenticity — not a
decentralized sequencer.
