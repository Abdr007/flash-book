# ER trust boundary & maturity closure (security rating item #3)

Deep treatment of the maturity gap from the security rating: *"devnet, unaudited;
the ER trust boundary is exercised on the live rollup rather than the unit harness;
existing markets need arming."* This separates what is **already proven in-harness**
from what **genuinely needs a live ER**, and gives the concrete closure plan.

---

## 1. The ER trust model — precisely what is and isn't trusted

flash-book runs the hot path (matching) on a MagicBlock Ephemeral Rollup: the
market book + commitment ring + (now) fill-outbox are *delegated* to the DLP
(`DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`), matched on the ER validator, and
periodically *committed* back to L1. The ER validator/sequencer is **semi-trusted**:
trusted for **ordering and liveness**, NOT for **correctness or custody**. Every way
a malicious or buggy ER operator could cheat is closed by a base-layer defense:

| If the ER operator tries to… | …it is stopped by | Tested |
|---|---|---|
| **Fabricate / alter / reorder a fill** | §3.2 fill-commitment ring — the matcher commits `keccak(fill_preimage)` on the ER; `apply_fill` recomputes and pops FIFO. Mandatory on armed markets. | Kani (`ring_push`/`ring_settle`) + integration (`fill_commitment_honest_path…`, omit-ring C-1 reject) |
| **Skim the JIT rebate** (flip `taker_was_jit`) | `taker_was_jit` bound into the committed preimage (§3.2 P1) | host (`preimage_binds_taker_was_jit`) |
| **Commit a corrupt book** (drive a slab accessor out of bounds → panic/DoS, or misread a node) | ER L-2: `from_account_data` bounds-checks all 6 header node indices (NIL or node-aligned in-bounds); fail-closed `OutOfRange` | host (`from_account_data_rejects_corrupt_node_index`) |
| **Replay / reorder a settled batch** | H1 monotonic `fill_seq` (Kani-proven `advance_settlement_seq`) | host + integration |
| **Drop a fill off the outbox** | outbox `cap >= ring_cap` + ring backpressure (Kani `outbox_no_silent_overwrite`) | Kani + integration (`fill_outbox_deep_sweep_256`) |
| **Censor settlement forever** (hold the book hostage on the ER) | permissionless `force_undelegate_market_book` once the ER is stall/censorship-dark | host (7 cases) — see below |
| **Trap pre-upgrade markets** (baseline-0 forever) | `book_delegated_at_slot` baseline + the F1/F2/F3 heartbeat logic | host (F1/F2/F3 cases) |
| **Steal custody** | the DLP owns the *delegated PDA*, never the vault; collateral never leaves L1 | by construction |

**The residual trust is liveness only:** a dead/withholding ER halts *new* matching,
but cannot take funds or forge state — and the censorship escape returns the book to
L1. That is the correct trust posture for an ER, and every safety defense above has a
base-layer choke point.

## 2. Test-coverage map — what's proven where, and why

The honest split the rating flagged:

**Tier 1 — proven IN THE UNIT HARNESS (host + `solana-program-test`).** Every
*safety* defense is here, because each is a pure function or a base-layer handler
the harness can drive directly:
- The fill-authenticity ring (Kani + integration honest-path + C-1 omit-reject).
- The ER L-2 corrupt-state rejection — *one choke point* (`from_account_data`) that
  **every** book-reading handler funnels through, so the host test covers the
  defense for all of them at once. A malicious committed book is rejected before any
  slab access, no panic.
- The `fill_seq` replay guard, the outbox no-overwrite proof, the JIT-bind.
- `force_undelegate_allowed` — the permissionless escape gate — 7 cases covering the
  FAST stall path, the BACKSTOP censorship path, the F1/F2/F3 heartbeat fixes, and
  the strict-inequality boundaries. The security-critical pure logic is fully
  covered.
- ER auth: `er_delegation_rejects_non_authority`, the ER-margin attestation suite
  (epoch-replay, wrong-attestor, xdomain-withdraw, session-funds).

**Tier 2 — genuinely needs a LIVE ER (not solana-program-test).** Only the
MagicBlock **CPI round-trips**, because the harness loads neither the DLP
(`DELeGG…`) nor the Magic program (`Magic111…`):
- `delegate_*` → DLP (account staging, ownership handoff, the `Delegate`
  discriminator CPI).
- `commit_*` / `commit_and_undelegate_*` → Magic program (`ScheduleCommit` /
  `ScheduleCommitAndUndelegate`).
- `process_undelegation` *as invoked by the DLP* (the base-layer callback that
  reopens the PDA from the DLP's buffer — the pure buffer-copy is host-tested, but
  the DLP-as-caller round-trip is not).
- The matcher actually executing on the ER validator.

**The key point for an auditor:** Tier 2 is a *CPI-shape / integration-with-MagicBlock*
question, not a *flash-book-logic* question. The byte construction of those CPIs is
covered by pure helpers (`cpi_delegate`/`cpi_commit`, the PDA derivations, the
`EXTERNAL_UNDELEGATE_DISCRIMINATOR == sha256("global:process_undelegation")[..8]`
identity). What can't be unit-tested is "does the real DLP accept our bytes" — and
that is validated on devnet, not asserted in CI.

## 3. How Tier 2 is validated today (and how to make it rigorous)

**BUILT (2026-06-29): `er-acceptance/`** — a reproducible, `ER_RPC`-gated live-ER
acceptance suite (skips cleanly without `ER_RPC`, like the SBF benches without
`BPF_OUT_DIR`). Its first run against the devnet MagicBlock ER
(`magicblock-core 0.13.2`) **validated the delegate-CPI round-trip for the book +
§3.2 ring** and immediately found two integration facts no unit/integration test
could (they don't delegate): **(1)** the 256-slot outbox CANNOT be ER-delegated —
the delegate-buffer is created at the full 24,640 B in one CPI, over the 10,240 B/ix
cap, so the deep outbox is **L1-only** under the current ring cap (book + ring
delegate fine, so §3.2 authenticity round-trips on the ER); **(2)** routing —
`null`-validator delegation must be transacted against the validator that claimed
the account (or the MagicBlock router), else the ER match returns
`InvalidWritableAccount`. This is precisely the Tier-2 value: real CPI-surface
findings the harness structurally cannot produce.

Prior to this it was only **devnet ER replays** (`scratchpad/replay/15_delegate`,
`17_smoke_force`, `18_wait_force`) and the live V2 ER census — ad-hoc. The suite
asserts one full round-trip per delegated account type:

```
delegate_market_book + delegate_fill_commitment + delegate_fill_outbox (together)
  → match a taker on the ER (commitments pushed, outbox written on the ER)
  → commit_* (snapshot to L1) → assert L1 sees consistent (book, ring, outbox)
  → commit_and_undelegate_* → process_undelegation finalizes on L1
  → assert the L1 book/ring/outbox are valid (from_account_data accepts) and settle
```
This belongs in a `tests/er_live/` suite gated behind an `ER_RPC` env var (skips in
CI like the SBF benches skip without `BPF_OUT_DIR`), so it's reproducible on demand
and becomes the Tier-2 gate before mainnet. It does NOT replace the Tier-1 proofs —
it covers exactly the CPI round-trips the harness structurally cannot.

## 4. Arming existing (grandfathered) markets — runbook

§3.2 P2 made arming the default for **new** markets (`initialize_market_inner` sets
`fill_commitment_required = true`). Markets created before that are grandfathered
(flag `false`, legacy optional behaviour). Arming them is operational, must be
**coordinated with the off-chain sequencer**, and is base-layer (the ring/outbox
can't be created while delegated). Per market:

1. **Undelegate** if delegated: `commit_and_undelegate_market_book` on the ER →
   `process_undelegation` finalizes on L1. (init/grow require base-layer ownership.)
2. **Arm the ring:** `init_fill_commitment` — creates `[fill_commit, market]` (cap
   256) and **sets `fill_commitment_required = true` (sticky)**. From this point
   `apply_fill` on this market REQUIRES the ring → the sequencer MUST start passing
   it. **Coordinate:** the sequencer must already be pushing commitments via the
   matcher (it does automatically once the ring account exists) and passing the ring
   to `apply_fill` *before* this flips, or settlement halts (`FillCommitmentMissing`).
3. **(Optional) Arm the outbox** for the 256 cap: `init_fill_outbox(max_batch_orders)`
   then `grow_fill_outbox` ×2 → 256 (the create-small-then-grow lifecycle). Until the
   outbox covers the ring the matcher is fail-closed on outbox markets — so grow to
   256 **before** raising real traffic, and the sequencer must be on the P-D read
   path (`SEQUENCER_OUTBOX_CUTOVER.md`).
4. **Re-delegate** (if the market runs on the ER): `delegate_market_book` +
   `delegate_fill_commitment` + `delegate_fill_outbox` together.
5. **Verify on-chain:** `fill_commitment_required == true`, the ring/outbox accounts
   exist + validate, and a test taker settles end-to-end.

**Safety:** every step fails closed. The dangerous-looking moment (step 2 flipping
the sticky flag) is exactly the H-2 guard — once armed, an un-armed settlement is
*rejected*, never silently accepted. Sequence: arm on a quiet market, confirm the
sequencer settles a probe fill through the ring, then resume traffic. The devnet
smoke (`23_outbox_smoke.mjs`) already demonstrates steps 2–3 + settle on a fresh
market; the same sequence applies to an existing one after step 1.

## 5. The other two maturity items (status + path)

- **External audit.** The single highest-leverage item; nothing above substitutes
  for it. The turnkey package (`SECURITY_REMEDIATION_2026-06.md`, 46 Kani proofs,
  Lean, 446+68 tests, this doc) is audit-ready. Action: engage a firm; the Tier-1/2
  split here tells them exactly where to focus (the live-ER CPI surface).
- **Decentralize authority.** A single key is upgrade authority *and* per-market
  authority + sequencer. The code already supports separation —
  `set_market_sequencer` (rotate the settlement key independently),
  `renounce_market_authority` / the burn ladder (drop market authority while keeping
  settlement live), and the upgrade authority can move to a multisig/timelock
  (BPFLoaderUpgradeable `set-upgrade-authority`). Action sequence before mainnet:
  (a) move upgrade authority → squads/timelock multisig; (b) set each market's
  sequencer to the operations key; (c) move market authority → multisig; (d)
  optionally burn market authority once oracle/params are final. Each is a single
  transaction; the on-chain support exists — it's an operational decision, not a
  code gap.

## 6. Bottom line

The ER trust boundary's **safety** is defended at base-layer choke points that are
**already unit-proven** (fill authenticity, corrupt-state rejection, replay,
no-overwrite, censorship escape — Kani + host + integration). What is *not* in the
unit harness is exactly the set of **MagicBlock CPI round-trips**, which are a
live-ER integration concern by nature; the closure is a gated `ER_RPC` acceptance
suite (§3), not more unit tests of logic that's already covered. Combined with the
external audit and authority decentralization (§5) — both supported by the code and
blocked only on operational decisions — this is the full, concrete path from the
current ~7/10 operational rating to production-grade.
