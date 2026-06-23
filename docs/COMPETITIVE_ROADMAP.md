# Flash Book — Competitive Roadmap: the best on-chain perps orderbook

Synthesis of a deep multi-agent analysis (audit · ER lifecycle · CU · quasar/pinocchio
port · qedsvm/qedgen formal verification) vs. the strongest public references
(the reference ER-orderbook implementations, Phoenix, Manifest). Goal: the orderbook Flash
Trade ships. **What's DONE is tested-green; what's PLANNED needs the noted toolchain/
runtime to execute — stated honestly, not as done.**

---

## 0. Honest scoreboard — where flash-book already wins

| Axis | flash-book | reference implementations | Verdict |
|---|---|---|---|
| **Correctness** | every CRITICAL+HIGH fixed (16 findings), 566 tests | — | ✅ **ahead** |
| **Feature breadth** | 16+ order types, full perp risk engine | thin (FIFO + external Percolator) | ✅ **ahead** |
| **CU (measured)** | place **12.5k**, beats Phoenix 18k / Manifest 14k | a native zero-copy CLOB reference has **NO measured CU** (2k-LOC scaffold, "extreme CU" is a doc-comment aspiration, not even on quasar) | ✅ **ahead** |
| **ER lifecycle** | **WIRED** — delegate→commit/commit+undelegate→`process_undelegation` callback | battle-tested receipt-action | ✅ **closed the gap** |
| **Formal proofs** | strong proptests (~65 props × 2000) + Certora hooks + runtime invariants | qedsvm 142 Hoare triples, an external risk engine 81 Kani, a Manifest-on-ER reference Certora suite + audit PDF | ⚠️ **the one real gap → plan below** |
| **Privacy (TEE)** | none | TEE dark-pool ER | ⚠️ his lead (largest effort; out of near-term scope) |

**Bottom line:** flash-book is already the better *exchange* and already CU-ahead of the
public references on the metric that matters (measured). The remaining work *extends*
a lead, not closes a deficit — except formal proofs, where there's a concrete plan to
match/exceed every tier.

---

## 1. CU — "hyper efficient" path (already ahead; here's how to extend)

flash-book already did ~80% of the zero-copy work Anchor leaves on the table (book =
raw `bytemuck` over `UncheckedAccount`; matcher = `no_std`-style; ER = hand-rolled CPI).
The residual Anchor overhead is narrow and specific.

**Phase 0 — targeted Anchor wins (low risk, NO framework change, ~25%):**
- Drop `Account<MarketAccount>` Borsh deser on place/taker/cancel → `UncheckedAccount` +
  manual disc read (mirror `MarketBookHandle`). **Also closes ER-security M-3** (add the
  `market_book.owner == crate::ID` check). ≈ −1–3k CU/ix.
- Replace the 3 hot-path `emit!`s with raw `sol_log_data` / drop redundant fields. ≈ −150 CU/ix.
- Bound the taker-walk `Vec<FillEntry>` with `heapless::Vec<_, MAX_BATCH>` (kill heap-grow).
- RBT: batched setters (fetch node once — M-2), let the RBT maintain its own `max_index`
  (drop the handle's separate successor pre-capture — D-1/D-2).
- **Targets:** place 12.5k→~9.5k, cancel 7.3k→~4.5k, taker_10 21.7k→~17k. *Beats Manifest.*
- **Gate:** CU savings are model-estimated — **must be measured** with the benchmark
  validator (the harness in `benchmark-results.json`) before claiming them.

**Phase 1+ — quasar / pinocchio port (bigger, −30–50% hot path):** the book+matcher core
is ~80% mechanical to port (it's already pointer-cast Pod). **But** quasar is unaudited
Beta v0.0.0; pinocchio is the raw substrate. For a perps DEX moving real value, ship the
**audited Anchor path (Phase 0) first**; do a quasar PoC on the book entrypoint, measure,
and only commit if it beats Phase 0 by a real margin. Recommended end-state: **hybrid** —
hot path on quasar, the 17k-LOC risk/settlement engine stays on audited Anchor behind CPI.

---

## 2. Formal verification — close the one real gap (proofs)

qedsvm = (1) executable SVM interpreter that runs the **real `flash_book.so`** agave-
conformant (exact CU) + (2) separation-logic proof layer (Hoare triples w/ CU bounds).
Honest caveat: the `.so`→theorem link is transcription-trusted; whole-program proofs are
not realistic. So field **all four tiers**, keyed to one spec (the qedgen pattern):

`proptest (have) → Kani (add) → Lean (add) → qedsvm binary CU+behavior (add)`

| Tier | Tool | flash-book status |
|---|---|---|
| Binary CU + behavior | **qedsvm** | add — Tier-1 `diff_mollusk` exact-CU on the `.so`; Tier-2 triples on `cancel`/`place_limit` |
| End-to-end invariants | **Certora** | hooks only — write the CVL (`oi_long==oi_short`, no-bad-debt, RBT order/black-height); reconcile the out-of-sync FBA doc to the continuous-CLOB code |
| Bounded model checking | **Kani** | add — target **>81 harnesses** (exceed the public bar); pure-math modules make it cheap |
| Randomized properties | **proptest** | **strong already** (~65 props × 2000) |

**Highest-ROI first proof:** haircut conservation `credit + dust == matured` and
`Σ credit ≤ Residual` (`haircut.rs:139,109`) as a 3-tier stack (Lean theorem + Kani
harness + the *existing* proptest `proptest_haircut.rs:88`). Pure integer math, solvency-
critical, ~1 week, fully discharged. Then the matcher base-conservation per-fill + the
Tier-1 exact-CU conformance headline.

**Composite claim it yields (spans every proof tier):** "*N* Lean universal
proofs + *>81* Kani harnesses + a full Certora conservation/solvency suite + agave-
conformant exact CU measured on the compiled `flash_book.so`, with haircut + matcher
conservation formally proven." Document the honest caveats (transcription gap, per-path
loops, Clock/CPI sysvar stubs) — don't over-claim "the whole program is proven."

**Toolchain gate:** Kani/Lean/Certora are not installed in the current env; this is a
multi-week effort that runs where those toolchains live.

---

## 3. ER lifecycle — DONE (the ER-lifecycle-closer)

Implemented + on-chain-compiling (566/566 tests, build-sbf clean):
- `commit_market_book` / `commit_and_undelegate_market_book` (ER-side, CPI the Magic program)
- `process_undelegation` (base callback — discriminator auto-derived = the exact
  `EXTERNAL_UNDELEGATE_DISCRIMINATOR` MagicBlock sends; verified)
- `er::cpi_commit` + `process_external_undelegate` + `create_pda` (hand-rolled, 2.1-clean)

**Remaining:** the **devnet-ER 7/7 lifecycle test** (init→delegate→match→commit→
base-reconcile→undelegate) — the only piece a live MagicBlock ER is needed for.
Also decide **delegated-unit coherence** (delegate per-user ledgers alongside the book,
like the reference implementations, so fund-locking also runs on the ER).

---

## 4. Suggested execution order

1. **Phase 0 CU** (in-repo, safe, measurable with the bench harness) — also fixes M-3.
2. **Formal Phase 1** (haircut + risk Kani/Lean, >81 harnesses) — exceeds the public Kani-harness bar cheaply.
3. **ER devnet 7/7 test** — finalizes the ER-lifecycle claim.
4. **Certora CVL** (Phase 2) — a Manifest-equivalent CVL suite + audit doc.
5. **qedsvm Tier-1/2** — the binary-CU + Hoare-triple headline.
6. (Optional) **quasar hot-path port** — only if Phase 0 + a measured PoC justify it.

Each is gated on its toolchain/runtime; none should ship a claim it can't measure or prove.
