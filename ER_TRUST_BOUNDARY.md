# The ER trust boundary

Flash Book matches on a MagicBlock Ephemeral Rollup and settles on Solana L1.
This document states precisely what the ER operator is and is not trusted
with, how each trust claim is enforced and tested, and which closure steps
are operational rather than code.

---

## 1. The trust model — the single-sequencer assumption, stated plainly

The market book, fill-commitment ring, and fill outbox are *delegated* to the
MagicBlock delegation program (`DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`),
matched on the ER validator, and periodically *committed* back to L1.

**Flash Book runs a single-sequencer trust model, by deliberate design.**
Operating a dedicated decentralized validator set was evaluated and ruled out
for this stage. The assumption is bounded and precise:

- **Trusted for ordering and liveness.** The sequencer decides which orders
  match, in what order, and whether matching happens at all.
- **NOT trusted for correctness or custody.** Fund-safety of settlement never
  rests on the sequencer: every fill it settles on L1 must verify against the
  keccak commitment the matcher pushed on the ER (the commitment ring —
  [docs/SETTLEMENT.md](docs/SETTLEMENT.md)), and collateral, positions, and
  the vault never leave L1.

This is an accepted, documented trust assumption — not an open gap and not a
solved problem. A sequencer-committee primitive exists on-chain
(quorum-attested batch roots, equivocation slashing —
[docs/DECENTRALIZED_SEQUENCER.md](docs/DECENTRALIZED_SEQUENCER.md)) but fill
settlement authorization is single-sequencer today, and any audit or
integration review should treat it as such.

Every way a malicious or buggy ER operator could go beyond that boundary is
closed by a base-layer defense:

| If the ER operator tries to… | …it is stopped by | Tested |
|---|---|---|
| **Fabricate / alter / reorder a fill** | the fill-commitment ring — the matcher commits `keccak(fill_preimage)` on the ER; `apply_fill` recomputes and pops FIFO; mandatory on armed markets | Kani (ring push/settle) + integration (honest path, omit-ring reject) |
| **Skim the JIT rebate** (flip `taker_was_jit`) | `taker_was_jit` is bound into the committed preimage | host (`preimage_binds_taker_was_jit`) |
| **Commit a corrupt book** (drive a slab accessor out of bounds, or misread a node) | `from_account_data` bounds-checks all six header node indices and the internal RBT links; fail-closed `OutOfRange` | host (corrupt-index and corrupt-link rejection) |
| **Replay / reorder a settled batch** | monotonic `fill_seq` (Kani-proven `advance_settlement_seq`) | host + integration |
| **Drop a fill off the outbox** | outbox `cap >= ring_cap` + ring backpressure (Kani: no silent overwrite) | Kani + integration (256-deep sweep) |
| **Censor settlement forever** (hold the book hostage on the ER) | permissionless `force_undelegate_market_book` once the ER is stall- or censorship-dark | host (seven cases) + Kani (never fires while live) |
| **Trap a market delegated before liveness stamping** | `stamp_book_liveness_baseline` + the two-tier stall/censorship baseline | host |
| **Steal custody** | the delegation program owns only the *delegated PDAs*, never the vault; collateral never leaves L1 | by construction |

**The residual trust is liveness only:** a dead or withholding ER halts *new*
matching but cannot take funds or forge state, and the censorship escape
returns the book to L1 without the sequencer's cooperation.

## 2. What is proven where

**Tier 1 — proven in the unit harness (host + `solana-program-test`).**
Every *safety* defense above is here, because each is a pure function or a
base-layer handler the harness drives directly: the fill-authenticity ring,
the corrupt-state rejection (one choke point — `from_account_data` — that
every book-reading handler funnels through), the `fill_seq` replay guard,
the outbox no-overwrite proof, the JIT bind, the force-undelegate gate, and
the ER-margin attestation suite (epoch replay, wrong attestor,
cross-domain withdraw, session funds).

**Tier 2 — needs a live ER.** Only the MagicBlock CPI round-trips, because
the harness loads neither the delegation program nor the Magic program:
`delegate_*`, `commit_*` / `commit_and_undelegate_*`,
`process_undelegation` as invoked by the delegation program, and the matcher
executing on the ER validator. The byte construction of those CPIs is
host-tested (`cpi_delegate`/`cpi_commit`, the PDA derivations, the
`EXTERNAL_UNDELEGATE_DISCRIMINATOR == sha256("global:process_undelegation")[..8]`
identity); what cannot be unit-tested is "does the real delegation program
accept these bytes" — and that is validated on devnet, not asserted in CI.

## 3. Live-ER validation

`er-acceptance/` is a reproducible, `ER_RPC`-gated acceptance suite that runs
the full round-trip against the live MagicBlock devnet ER: delegate (market +
book + ring + outbox) → match on the rollup (commitments + outbox written on
the ER) → `commit_*` → assert the ring and outbox cursors survived →
`commit_and_undelegate_*` → `process_undelegation` → assert every account is
back under the program and structurally valid. It skips cleanly when
`ER_RPC` is unset, so CI never depends on an external rollup.

Live integration facts the suite enforces (discoverable only against the
real ER):

- A 256-slot outbox cannot be ER-delegated (the delegate-buffer create
  exceeds 10,240 bytes per instruction) — hence the per-market cap knob:
  cap ≤ 105 is fully ER-capable; up to 256 is L1 deep-sweep.
- The delegation must pin the validator identity; a `null` validator leaves
  the owning validator ambiguous.
- The `market` account must be delegated alongside book/ring/outbox — the ER
  rejects a writable mix of delegated and undelegated accounts.

## 4. Arming a grandfathered market — runbook

New markets arm the fill-commitment ring at creation
(`fill_commitment_required = true`). A market created before arming became
the default must be armed operationally, coordinated with the off-chain
sequencer, on the base layer:

1. **Undelegate** if delegated: `commit_and_undelegate_market_book` on the ER
   → `process_undelegation` finalizes on L1.
2. **Arm the ring:** `init_fill_commitment` creates `[fill_commit, market]`
   and sets the sticky `fill_commitment_required`. From this point
   `apply_fill` on this market requires the ring — the sequencer must already
   be passing it, or settlement halts fail-closed (`FillCommitmentMissing`).
3. **(Optional) Arm the outbox** for deep sweeps: `init_fill_outbox`, then
   `grow_fill_outbox` to the target cap; the sequencer must be on the
   outbox read path ([docs/SETTLEMENT.md](docs/SETTLEMENT.md) §4).
4. **Re-delegate** (if the market runs on the ER): `delegate_market_book` +
   `delegate_fill_commitment` + `delegate_fill_outbox` together.
5. **Verify:** `fill_commitment_required == true`, the ring/outbox accounts
   validate, and a probe taker settles end-to-end.

Every step fails closed: once armed, an un-armed settlement is rejected,
never silently accepted. Arm on a quiet market, probe, then resume traffic.

## 5. Closure items that are operational, not code

- **External audit.** The highest-leverage remaining item; nothing in this
  document substitutes for it. The Tier-1/Tier-2 split above tells an
  auditor exactly where to focus (the live-ER CPI surface).
- **Authority decentralization.** The code supports separated keys
  (`set_market_sequencer`, 2-step authority transfer, timelocked params,
  authority burn) and multisig authorities; moving the live keys to an
  M-of-N multisig is an operational migration —
  [docs/OPERATIONS.md](docs/OPERATIONS.md).

## 6. Bottom line

Safety at the ER boundary is defended at base-layer choke points that are
unit-proven and machine-checked. Ordering and liveness rest on a single
sequencer by explicit, bounded design, with a permissionless exit when it
fails. What remains outside the unit harness is exactly the set of
MagicBlock CPI round-trips, which the gated live-ER acceptance suite covers
on demand.
