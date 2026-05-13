# Commit-Reveal On Chain — Scope & Design

Companion to `docs/FBA_ON_CHAIN.md`. FBA gives **within-batch
permutation invariance** for plaintext orders. Commit-reveal goes
further: it hides **the order's intent itself** from the sequencer
and other traders until the batch closes. Together they close the
loop — the sequencer cannot reorder (FBA), and cannot inspect (CR)
the orders within a batch.

As of `v0.2.0`, the TypeScript simulator in `src/commit-reveal.ts`
has the protocol implemented. The on-chain Anchor program has no
commit-reveal accounts and no two-phase placement ixs.
`grep -rn 'commit_reveal' programs/flash-book/src/` returns zero
hits.

This document is the scope-discovery artifact for the on-chain
migration, same pattern as `SUB_ACCOUNT_TRADING.md` and
`FBA_ON_CHAIN.md`.

## 0. Status

**Not started.** Commit-reveal depends architecturally on FBA being
on-chain first — both work together. The "commit" phase makes
sense only if there's a discrete batch boundary at which to "reveal";
under continuous CLOB, every order is immediately revealed by virtue
of being matched.

So this is sequenced **after** `docs/FBA_ON_CHAIN.md` lands.

## 1. The protocol

Two phases per order:

### Phase A — commit (tx 1)

The trader signs and submits a `place_commit` ix carrying:

```
hash = blake3(side ‖ size ‖ limit_ticks ‖ flags ‖ expires_at_slot ‖ sub_index
             ‖ nonce ‖ trader.pubkey)
```

where `nonce` is a 64-bit value the trader generates locally. The ix
records the hash + a slashable bond + the submitting trader. The
order's actual parameters are NOT on-chain at this point — only the
hash is.

The commit goes into a per-market `CommitPool` PDA. While the batch
is open, anyone (including the sequencer) sees the hash but cannot
reverse-engineer the order without the nonce.

### Phase B — reveal (tx 2)

Within the reveal window (a fixed slot range after the batch closes
but before clearing), the trader submits a `place_reveal` ix carrying
the plaintext parameters + the nonce. The on-chain handler:

1. Re-computes the hash from the revealed parameters.
2. Asserts the recomputed hash matches the stored commit.
3. On match: the order is enqueued into the FBA `PendingBatchBuffer`
   as if it had been submitted plaintext, AND the bond is returned
   to the trader.
4. On no-match (or no-reveal within the window): the bond is
   slashed to the insurance fund. The order does NOT enter the
   batch.

### Phase C — batch clear

After the reveal window closes, `clear_batch` (from
`FBA_ON_CHAIN.md`) runs against the revealed orders. The orders that
made it through reveal participate in the Walrasian clearing.

## 2. Why slashing is necessary

Without a slashable bond, a malicious trader could:

1. Commit a set of orders covering the entire price range
   (effectively a "phantom order book" claim).
2. Watch the sequencer's pre-clearing state.
3. Selectively reveal only the commits that turn out to be
   profitable.

Slashing makes this unprofitable: every commit costs bond, and
not-revealing forfeits the bond. The bond size is calibrated against
the expected MEV value of a single order's information.

Hyperliquid's HyperBFT model uses validator consensus to enforce
similar properties without an explicit bond. Flash Book's design is
SPL-token-economic — the bond is real value at stake per commit.

## 3. State on-chain

### CommitPool (per market)

```rust
#[account]
pub struct CommitPool {
    pub bump: u8,
    pub market: Pubkey,
    pub batch_seq: u64,             // matches PendingBatchBuffer.batch_seq
    pub commit_window_close_slot: u64,
    pub reveal_window_close_slot: u64,
    pub commits_count: u16,
    pub _pad: [u8; 5],
    pub commits: [CommittedOrder; MAX_COMMITS_PER_BATCH],
}

#[derive(Clone, Copy, Pod, Zeroable)]
pub struct CommittedOrder {
    pub trader: Pubkey,
    pub hash: [u8; 32],
    pub bond_quote_lots: u64,
    pub submitted_at_slot: u64,
    pub revealed: u8,            // 0 = pending, 1 = revealed, 2 = slashed
    pub _pad: [u8; 7],
}
```

`MAX_COMMITS_PER_BATCH` matches `MAX_PENDING_PER_BATCH` from FBA
(default 256). Bond stored separately so the slash path can refund
or burn cleanly.

### Bond escrow

Bonds are held in `InsuranceFundAccount.commit_bonds_pool` (a new
field). On commit, the trader transfers `bond_quote_lots` from their
`trader_state.collateral_quote_lots` into this pool. On successful
reveal, the bond returns. On no-reveal / hash-mismatch, the bond
moves into `InsuranceFundAccount.balance_quote_lots`.

## 4. Ix surface

```
place_commit(
    market: Pubkey,
    hash: [u8; 32],
    bond_quote_lots: u64,
) -> reserves bond, records hash, sets reveal deadline

place_reveal(
    market: Pubkey,
    side: u8,
    size_lots: u64,
    limit_ticks: u64,
    flags: u8,
    expires_at_slot: u64,
    sub_index: u8,
    nonce: u64,
) -> verifies hash, enqueues to PendingBatchBuffer, refunds bond

slash_unrevealed_commits(market: Pubkey)
  -> permissionless; sweeps any CommittedOrder where
     reveal_window_close_slot < current_slot AND revealed == 0;
     moves their bonds to InsuranceFundAccount.balance_quote_lots
```

## 5. Timing diagram

```
slot 0           : batch opens; commits accepted
slot 100          : commit window closes; reveals accepted
slot 110          : reveal window closes; revealed orders are now
                    in PendingBatchBuffer
slot 110 + 1      : clear_batch runs; revealed orders Walrasian-cleared
slot 111+         : slash_unrevealed_commits sweeps phantom bonds
```

Concrete cadence is per-market via two new MarketParams:
`commit_window_slots`, `reveal_window_slots`. Reasonable defaults:
50 ms commit / 50 ms reveal at Solana's 400 ms slot, so ~1 slot each.

## 6. Hash construction (audit-critical)

```
hash = blake3(
    domain_separator     "fb.cr.v1"
  ‖ market               (32 bytes)
  ‖ batch_seq            (8 bytes, LE)
  ‖ trader               (32 bytes)
  ‖ side                 (1 byte)
  ‖ size_lots            (8 bytes, LE)
  ‖ limit_ticks          (8 bytes, LE)
  ‖ flags                (1 byte)
  ‖ expires_at_slot      (8 bytes, LE)
  ‖ sub_index            (1 byte)
  ‖ nonce                (8 bytes, LE)
)
```

Domain separation: the prefix `"fb.cr.v1"` prevents cross-context
hash collisions (e.g. someone hashing the same bytes for a different
purpose accidentally matching a commit). The `batch_seq` and `market`
inclusion prevent replay across batches or markets.

Blake3 is chosen because it has SIMD-friendly Rust implementations
that compile to BPF without issue. SHA-256 also works but is slower.

## 7. Properties for proptests

```
tests/proptest_commit_reveal.rs
  1. hash_uniqueness — two different orders never collide (random
     pair × 2000 cases).
  2. reveal_idempotence — a revealed order's effect on the batch is
     identical to a plaintext-submitted equivalent (Phase 2j-style
     parity check).
  3. slash_correctness — every CommittedOrder past reveal_window
     with revealed == 0 has its bond redirected to insurance, with
     conservation: sum_bonds_in + sum_bond_returns + sum_slashed_to_insurance
     == sum_bonds_committed.
  4. nonce_reuse_safety — a trader who reuses (batch, nonce) across
     orders gets the second attempt rejected (same hash, but
     CommittedOrder.revealed != 0 OR commit already exists).
```

## 8. Effort estimate

| Slice | LOC | Notes |
|---|---|---|
| `CommitPool` state + `CommittedOrder` struct | ~150 | bytemuck-pod, init ix, slot bookkeeping |
| `place_commit` ix | ~200 | Hash storage, bond escrow CPI, slot windows |
| `place_reveal` ix | ~250 | Hash verification, parameter validation, enqueue into PendingBatchBuffer (requires FBA shipped first), bond refund |
| `slash_unrevealed_commits` ix | ~150 | Permissionless sweep, conservation accounting |
| Blake3 import + BPF audit (constant-time, no SIMD assumption mismatch) | ~50 | crate selection |
| Bond escrow integration with InsuranceFundAccount | ~100 | New field; migration path for existing fund |
| `MarketParams.commit_window_slots` + `.reveal_window_slots` | ~50 | Init + update |
| Proptests (4 properties × 2000 cases) | ~300 | Hash, idempotence, slash, nonce safety |
| Integration tests (5 scenarios) | ~500 | Honest commit+reveal, no-reveal slash, malformed reveal, replay attempt, MEV-resistance scenario |
| SDK builders + nonce-generation helper | ~200 | Includes off-chain hash compute parity helper |
| Docs | ~150 | MATH.md / ARCHITECTURE.md updates |
| **Total** | **~2,100** | |

Earliest target release `v0.4.0` (after FBA in v0.3.0).

## 9. Threats this primitive defeats

- **Sequencer-side sandwich attacks within a batch.** The sequencer
  can no longer see what trades are coming until reveal — by which
  point reordering is impossible (the commit's hash is fixed; the
  sequencer can't substitute a different order).
- **Trader-side adverse-selection leakage.** Two traders can place
  cancelling orders without leaking either's intent. Their commits
  look identical hashes (different nonces); the reveals expose
  intent simultaneously.
- **Front-running via mempool inspection.** Solana mempools are
  visible; under commit-reveal, only hashes are visible until
  reveal, so the attacker has nothing to front-run on.

## 10. Threats this primitive does NOT defeat

- **Reveal-time MEV.** Once a trader reveals, the order is in the
  open until the batch clears. If the reveal window is long enough
  (say 1 second on Solana), a fast attacker could observe a reveal
  and submit their own commit in a still-open window. **Mitigation:**
  the on-chain dispatch is `place_commit` → wait → `place_reveal` →
  `clear_batch`. Once `clear_batch` is queued, no more commits land
  in that batch. The window math must guarantee a short-enough commit
  window that observed-reveal-as-signal is unprofitable.
- **Cross-protocol MEV.** A trader who reveals at Flash Book can
  still be front-run on Phoenix or Drift. Commit-reveal protects
  intent within Flash Book's batch only.
- **The chain operator slashing bonds.** If the chain operator
  (Solana validators) censor reveals while accepting commits, all
  bonds get slashed. The mitigation is the same as for any
  validator-censorship attack on Solana: rely on Solana's own
  decentralisation.

## 11. Dependency graph

```
FBA_ON_CHAIN.md (v0.3.0)
  └─→ COMMIT_REVEAL_ON_CHAIN.md (v0.4.0)
        ├─→ requires PendingBatchBuffer (FBA state)
        ├─→ requires batch_seq from FBA
        └─→ requires clear_batch ix from FBA
```

Commit-reveal cannot ship before FBA. Both should land via the same
audit pass since they compose tightly and the security claims are
joint.

## 12. Versioning

This document is the scope-discovery artifact for commit-reveal
on-chain. When the work ships, the implementing commits reference
back here, and the sections become "SHIPPED" the same way Phase 2
sections did in `SUB_ACCOUNT_TRADING.md`.

Until then, the COMPARISON.md "honest weaknesses" section accurately
states that no on-chain commit-reveal exists in the deployed code.
