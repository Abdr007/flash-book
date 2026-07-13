# The ER trust boundary

Clober matches on a MagicBlock Ephemeral Rollup and settles on Solana L1.
This document states precisely what the ER operator is and is not trusted
with, how each trust claim is enforced and tested, and which closure steps
are operational rather than code.

---

## 1. The trust model — the single-sequencer assumption, stated plainly

The market book, fill-commitment ring, and fill outbox are *delegated* to the
MagicBlock delegation program (`DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`),
matched on the ER validator, and periodically *committed* back to L1.

**Clober runs a single-sequencer trust model, by deliberate design.**
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
| **Censor settlement forever** (hold the book hostage on the ER) | *intended:* permissionless `force_undelegate_market_book` once the ER is stall- or censorship-dark. **Not executable today** — the deployed MagicBlock delegation program makes undelegation validator-driven and exposes no owner-callable path, so the handler runs the (Kani-proven) liveness gate and then fails closed with `OwnerForceUndelegateUnavailable`. The working exit is sequencer-gated `commit_and_undelegate_market_book`. See §1.1. | host (seven cases, gate) + Kani (gate never fires while live) |
| **Trap a market delegated before liveness stamping** | `stamp_book_liveness_baseline` + the two-tier stall/censorship baseline | host |
| **Steal custody** | the delegation program owns only the *delegated PDAs*, never the vault; collateral never leaves L1 | by construction |

**The residual trust is liveness — and, until the trustless escape is wired,
sequencer cooperation for exit.** A dead or withholding ER cannot take funds or
forge state (custody and positions stay on L1). But because the permissionless
`force_undelegate_market_book` escape is not currently executable against the
deployed delegation program (see §1.1), returning a censored/dark book to L1
today depends on the sequencer signing `commit_and_undelegate_market_book`. A
sequencer that is dead or refuses to sign therefore traps the book on the ER —
so open positions cannot be closed and the collateral backing them cannot be
withdrawn (withdrawal requires `open_positions == 0`) until the ER returns or
MagicBlock ships an owner-recovery instruction. Trapped-margin recoverability
rests on sequencer liveness, not a trustless exit. Nothing is stolen or forged;
the exposure is liveness/availability, not custody.

**One accepted correctness residual — reduce-only intent across the drain
window.** A reduce-only maker fill committed to the ring, whose maker
position is independently liquidated/ADL'd to flat on L1 before that fill
drains, settles into an opposite position — violating the reduce-only
*intent*, but not fund-safety: conservation and OI balance hold, and the
opened position is immediately liquidatable. It is not code-fixed because
the only closures break a hard wall (settlement-side clamp breaks
two-sided conservation; gating liquidation on a drained ring deadlocks
liquidation when the sequencer withholds a preimage). Detailed in
[docs/SETTLEMENT.md](docs/SETTLEMENT.md) §3.

### 1.1 Status of the trustless censorship escape

The permissionless exit is **designed and gated but not yet executable against
the deployed delegation program.** `force_undelegate_market_book` computes the
Kani-proven two-tier liveness gate (`force_undelegate_allowed`: a fast
stall/heartbeat path and a longer settlement-liveness censorship backstop) and,
when the gate opens, currently returns `OwnerForceUndelegateUnavailable` rather
than emitting a CPI. The reason is external: the upgraded MagicBlock delegation
program makes undelegation **validator-driven** — `process_undelegate` requires
the ER validator as a signer plus committed rollup state and exposes no
owner-callable undelegate path — so an owner-initiated force-undelegate has no
instruction to call and would be guaranteed to fail. The gate is retained so the
escape can be re-wired the instant MagicBlock ships an owner-recovery
instruction.

Consequences for the current deployment:

- The **only** path back to L1 today is `commit_and_undelegate_market_book`,
  which is sequencer-gated (`payer == market.sequencer`). Exit therefore
  depends on sequencer cooperation.
- A dead or censoring sequencer that refuses to sign it traps the delegated
  book on the ER. Custody is unaffected (the vault and collateral never leave
  L1; positions are read-only clones), but affected traders cannot close
  positions or withdraw the collateral backing them until the ER returns.
- This is a **liveness/availability** exposure, not a custody or correctness
  one, and it is the single most important item to close before any
  mainnet-scale claim of a permissionless exit. Closing it requires an
  owner-recovery primitive from MagicBlock (external), after which the retained
  gate wires directly to it.

### 1.2 Cross-domain reserved margin — withdraw anytime, and the attestation window

Collateral never leaves L1, but resting orders live on the delegated book, so
L1 cannot see the margin a live ER order will require when it fills. The
bridge is the per-trader `ErMarginAttestation`: the sequencer's pinned
attestor writes the total initial margin reserved by the trader's live ER
orders (`attest_er_reserved_margin`, strictly-increasing epoch, replay-proof),
and **every** collateral-releasing instruction — `withdraw_collateral` (and
its xdomain variant), both partial-withdraw variants, both sub-account
transfers, and `sweep_collateral` — enforces the same invariant:

```
withdrawable = collateral − max(IM_filled, floor) − er_reserved
```

Free balance is withdrawable at any time, mid-session, with open positions
and resting orders; the reservation stays behind. There is no arm step, no
lock, and no "turn off trading to withdraw." Order placement on a delegated
book requires the trader's attestation account to exist (`er_margin_ready`),
so the sequencer always has somewhere to write the reservation.

**The accepted residual is the attestation lag.** The reservation lands on L1
only when the sequencer attests, which is after an order rests. A trader who
places an ER order and withdraws before the next attestation can leave that
order's eventual fill under-margined — bounded by the raced amount and by one
attestation interval. Mitigation is cadence: the sequencer runs the
attestation cranker ([sequencer/](sequencer/README.md)), which reads the
delegated book every poll interval and attests any reservation change, so the
window is seconds. This is squarely inside the
already-accepted single-sequencer trust of §1 (the same operator is already
trusted for ordering and for honest fill commitment), not an expansion of it;
a sequencer that under-attests can cause bounded under-margining, never theft
(the vault only ever pays out against the trader's own balance). It is
documented here as an accepted residual — not solved. The trustless
replacement (order-book state proven onto L1 so the reservation needs no
attestor) is staged as future infrastructure and is explicitly out of scope
for the current deployment.

## 2. What is proven where

**Tier 1 — proven in the unit harness (host + `solana-program-test`).**
Every *safety* defense above is here, because each is a pure function or a
base-layer handler the harness drives directly: the fill-authenticity ring,
the corrupt-state rejection (one choke point — `from_account_data` — that
every book-reading handler funnels through), the `fill_seq` replay guard,
the outbox no-overwrite proof, the JIT bind, the force-undelegate gate, and
the ER-margin attestation suite (epoch replay, wrong attestor,
cross-domain withdraw and partial withdraw, reservation-gated sub-account
transfers and sweep, session funds).

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
- **Trustless censorship exit.** Wiring the gated `force_undelegate_market_book`
  escape to a real owner-recovery instruction (see §1.1). Until MagicBlock
  ships one, exit from a censored/dark ER depends on sequencer cooperation and
  trapped-margin recoverability rests on sequencer liveness.

## 6. Bottom line

Safety at the ER boundary is defended at base-layer choke points that are
unit-proven and machine-checked. Ordering and liveness rest on a single
sequencer by explicit, bounded design. The permissionless exit for a
censored/dark ER is designed and gated but **not yet executable** against the
deployed delegation program (§1.1), so exit currently depends on sequencer
cooperation — a liveness exposure, not a custody or correctness one. What
remains outside the unit harness is exactly the set of MagicBlock CPI
round-trips, which the gated live-ER acceptance suite covers on demand.
