# Decentralized sequencer — M-14 endgame (design)

> Status: **design**. This is the architecture + phased plan for removing the
> single-sequencer trust point (audit residual **M-14**) while keeping a
> **continuous, price-time CLOB** — explicitly **not** FBA. The full system is a
> multi-month distributed-systems buildout; this doc scopes it and specifies the
> concrete on-chain primitive so implementation can proceed in safe, verifiable
> phases. It does not itself change program behavior.

## 1. Goal & non-goals

**Goal.** No single party can choose fill ordering or nudge the mark within the
band. Trust moves from *one* sequencer to an *M-of-N* validator committee that
agrees on order flow via BFT consensus.

**Non-goals.**
- **No FBA / batch auctions.** Continuous matching with strict price-time priority
  is the product edge (sub-50ms, real time priority — the Hyperliquid/dYdX model,
  not the CoW/Injective batch model). Consensus decides the *input order*; matching
  stays continuous and deterministic.
- No change to the proven settlement math (`apply_fill`, haircut, margin, FLP). We
  change **who is authorized to settle**, not **how a fill settles**.

## 2. Where we are (and the exact residual)

Today, per market: one off-chain `sequencer` runs the continuous CLOB, pushes
`keccak` fill commitments (the §3.2 ring), and calls `apply_fill`.

Already enforced on-chain (so the residual is *narrow*):
- **Authenticity** — a sequencer cannot fabricate / alter / reprice a fill
  (commitment ring recompute at settlement).
- **No reorder / replay at settlement** — monotonic `fill_seq`
  (`advance_settlement_seq`, Kani-proven).
- **Mark pinned to the trustless oracle** — always-on ≤5% band clamp (#215).

**Residual (M-14):** a single sequencer still (a) chooses *which* crossable order
to service first among simultaneously-arriving orders, and (b) sets the mark
*within* the tight band. Both are irreducible without decentralizing *who
sequences*.

## 3. Target architecture (BFT-consensus continuous CLOB)

```
 traders ──orders──►  Validator set (N; f Byzantine, N ≥ 3f+1)
                         │  1. shared mempool of signed orders
                         │  2. BFT consensus on the ORDER-FLOW SEQUENCE
                         │     (HotStuff/HyperBFT-class; one leader per view,
                         │      2f+1 votes commit a block = an ordered batch)
                         │  3. each validator DETERMINISTICALLY replays the
                         │     agreed order sequence through the SAME CLOB
                         │     engine → identical fills + mark (state machine
                         │     replication)
                         │  4. 2f+1 validators threshold-sign the batch:
                         │     { prev_state_root, fills_merkle_root,
                         │       new_state_root, mark, epoch, seq }
                         ▼
                  L1 / ER settlement (flash-book program)
                   verify M-of-N committee sigs over the batch root,
                   then settle the batch (existing apply_fill math)
```

Because the *sequence* is consensus-determined and matching is a deterministic
function of it, no single validator controls ordering or the mark. Safety needs
2f+1 honest; a minority cannot forge a batch (can't reach threshold) nor reorder
(the committed sequence is signed).

## 4. On-chain changes (the flash-book program part — bounded & specifiable)

This is the part that lives in this repo. Four additions, all additive/versioned:

1. **Sequencer committee.** New PDA `SequencerCommittee { market, epoch,
   validators: Vec<Pubkey> (≤ MAX_N), threshold: u8, ... }`, or generalize
   `market.sequencer` → committee id. `threshold = 2f+1`.
2. **Threshold-attestation settlement.** A `settle_batch` path that verifies
   **≥ threshold** ed25519 signatures (Ed25519 precompile, batched — same
   introspection pattern as `update_oracle_from_lazer`) over
   `keccak(prev_state_root ‖ fills_merkle_root ‖ new_state_root ‖ mark ‖ epoch ‖
   batch_seq)`, then applies the fills via the **existing** `apply_fill` math.
   `batch_seq` is monotonic (reuse the `advance_settlement_seq` guard) — the batch
   analog of per-fill replay protection.
3. **State-root binding.** The batch carries `prev_state_root` / `new_state_root`
   (a commitment to the book/positions transition) so a partial or forged batch
   fails to chain — settlement advances only on a validly-signed, correctly-chained
   transition.
4. **Committee governance.** Authority (→ multisig/DAO) rotates the validator set
   at epoch boundaries: `set_committee(epoch, validators, threshold)`, with the old
   committee finalizing its last batch before handoff. Optional **staking/slashing**
   PDA: validators stake; two conflicting signed batches at the same `batch_seq` are
   slashable (submitted as a fraud proof).

None of this touches the settlement *math* or the FIFO ring semantics — it wraps
**authorization** in an M-of-N check instead of a single signer.

## 5. Off-chain (the multi-month buildout — NOT this repo)

- **Consensus engine.** A HotStuff/HyperBFT-class BFT (candidates:
  Malachite/Tendermint-core, or a purpose-built HotStuff-2). Leader per view,
  pipelined commits, sub-second finality.
- **Deterministic CLOB replica.** The exact matching engine, run identically by
  every validator over the agreed order sequence (state-machine replication). Must
  be byte-deterministic (fixed tick math, no wall-clock in matching).
- **Validator client + mempool + networking**, batch threshold-signing, L1/ER
  submission, epoch/rotation coordination.
- **Economics:** stake, rewards, slashing conditions, validator onboarding.

This is where the real engineering-months live.

## 6. Migration — phased, each phase shippable & safe

- **Phase 0 (done).** Single sequencer + authenticity + monotonic `fill_seq` +
  mark clamp. The residual is *bounded*, not open.
- **Phase 1 (on-chain primitive, backward-compatible).** Introduce the committee as
  **N = 1, threshold = 1** — functionally **identical** to today's single sequencer,
  so zero behavior change, but the settlement-authorization primitive and the batch
  path now exist. This is the first concrete PR (see §7). Fully CI + live-ER
  verifiable because 1-of-1 reproduces current behavior.
- **Phase 2 (on-chain).** Enable **N > 1, threshold = 2f+1** batch attestation +
  state-root chaining + governance rotation. Verify against a local multi-signer
  test harness.
- **Phase 3 (off-chain).** Build the BFT consensus engine + deterministic CLOB
  replica + validator client.
- **Phase 4.** Stand up the validator set on devnet, rotate 1-of-1 → M-of-N, run
  the live acceptance suite against the committee, then mainnet.

Each phase is independently reviewable; Phase 1 is safe *because* it's a no-op
functionally.

## 7. The concrete first PR (Phase 1)

Smallest safe step that lays the foundation without changing behavior:

- Add `SequencerCommittee` PDA (default `N=1` = the current `market.sequencer`).
- Add `settle_batch` that verifies `threshold` Ed25519 sigs over the batch root and
  dispatches to the existing per-fill settlement, with a monotonic `batch_seq`.
- Keep the existing single-signer `apply_fill` as the `N=1` fast path (no regression).
- Host tests: threshold verification (1-of-1 accept; 0 sigs reject; wrong-root
  reject) + `batch_seq` monotonicity (mirror `advance_settlement_seq` proofs).
- The live-ER acceptance suite runs unchanged (1-of-1 == today), proving no
  behavior change.

Estimated: a focused, well-tested PR — the on-chain scaffold for decentralization,
shippable through the same four-check CI + live-ER path as every Round-2 fix.

## 8. Security notes

- **Trust shift:** from "1 honest sequencer" to "≥ 2f+1 of N honest validators."
  Safety (no forged/reordered fills) holds under < N/3 Byzantine; liveness needs
  2f+1 responsive.
- **Ordering discretion → removed:** the committed order-flow sequence is signed by
  the quorum; a leader that censors/reorders is replaced by view-change and is
  slashable, so no single party sets ordering or the mark.
- **Composability:** the state-root chaining + monotonic `batch_seq` are the batch
  analogs of the fill-commitment authenticity + `fill_seq` we already prove — the
  security argument extends, it doesn't restart.
- **What it does NOT solve:** oracle trust (still Pyth/Lazer), the MagicBlock DLP
  escape hatch (M-16, upstream), and validator economics (a social/incentive layer).
