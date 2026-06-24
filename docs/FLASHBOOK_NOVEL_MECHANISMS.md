# Flash Book — Novel Mechanisms (Invention Disclosure)

> Technical disclosure of the novel, non-obvious mechanisms in the Flash Book
> on-chain CLOB perpetuals protocol, prepared for evaluation by patent counsel.
>
> **Disclaimer.** This document identifies *candidate* inventions — novel,
> useful, and (we believe) non-obvious technical mechanisms grounded in the
> actual implementation. **Patentability is a legal determination** (requiring a
> prior-art search and a novelty/obviousness analysis) that this document does
> **not** make. Every mechanism below is cited to real source (`file:line`) and,
> where stated, is **machine-checked** (Kani / Lean) — not aspirational.

**System context.** Flash Book is a fully on-chain central-limit-order-book
(CLOB) perpetual-futures DEX on Solana: a red-black "hypertree" order book with
price-time priority, a stress-lattice portfolio-margin risk engine, and optional
sub-50ms execution via MagicBlock Ephemeral Rollups. Program:
`programs/flash-book/src`.

---

## Invention 1 — Formally-proven junior-claim PnL-conservation engine ("H-Haircut")

**Field / problem.** In a perpetual-futures venue, realized *profits* of winning
traders are paid from a shared pool that also backs losing traders' positions and
liquidity. If the pool is depleted by bad debt (a position that gaps past its
collateral), naively paying out winners' full realized PnL makes the protocol
**insolvent** (it pays out value it does not hold). Existing designs socialize
this loss reactively (auto-deleveraging, insurance draws) *after* insolvency is
already possible.

**The mechanism.** Released positive PnL is treated as a **junior claim** rather
than immediately spendable collateral:

1. A trader's released positive PnL is parked in a per-position **reserve** and
   *matures* linearly over a warm-up window (`matcher::haircut::apply_release`;
   `state_v3::PositionHaircutStateAccount`).
2. Matured PnL is convertible to spendable collateral only through
   `convert_position`, and only at a **solvency factor**
   `h = min(Residual, MaturedTotal) / MaturedTotal`, where
   `Residual = V − C_tot − I` (total protocol assets minus committed collateral
   minus the insurance fund) — `matcher::haircut::compute_h`,
   `lib.rs convert_position`.
3. `Residual` is **delta-tracked** by every money-moving instruction — funding
   (`lib.rs settle_funding`), and now **realized losses credit it**
   (`lib.rs accrue_haircut_loss`, H7) while conversions debit it. The conversion
   credits the trader's collateral by exactly the haircut-adjusted amount, with
   the remainder accrued as dust (`lib.rs convert_position`, H9).

**Net effect:** the protocol **mathematically cannot pay out more realized gains
than its actual backing.** Bad debt is absorbed by *haircutting junior claims*
(`h < 1`), structurally preventing insolvency rather than reacting to it.

**Novelty / non-obviousness.** No surveyed on-chain perp (Drift, GMX, dYdX,
Hyperliquid, the surveyed reference implementations) implements a junior-claim
PnL tranche gated by a delta-tracked solvency factor. The non-obvious step is
making *released PnL itself* the junior tranche and proving conservation at the
real fixed-point divisor.

**Verification status — STRONGEST.** The conservation/solvency bound (`credit ≤
Residual`, `credit + dust == matured`, `h ≤ 1`) is **machine-proven in Lean 4 at
the real 1e9 divisor** — a regime where CBMC/Kani SAT backends are incomplete on
128-bit non-power-of-two division — plus **5 Kani proofs** for the bounded
properties (`matcher/haircut.rs`, `docs/FORMAL_VERIFICATION.md`). *No surveyed
perp has a formally-proven PnL-conservation mechanism.*

**Candidate claims (sketch).** (a) A method of crediting realized derivative
profit to collateral only at a solvency ratio derived from delta-tracked protocol
residual vs. outstanding matured claims; (b) treating released positive PnL as a
maturing junior tranche; (c) the conservation guarantee verified at the
production fixed-point divisor.

---

## Invention 2 — Dual-source "worse-of" liquidation gate with staleness refusal

**Field / problem.** A liquidation engine must decide a position is unhealthy.
Using the **mark price** alone (an EMA of fills) lets a thin-liquidity tape lag a
real move; using the **oracle** alone trusts a single feed that can gap or be
manipulated (the Mango / JELLY class). Existing perps pick one.

**The mechanism.** Position health is evaluated at the **worse of the two sources
for the position's direction** — `LONG: min(mark, oracle)`, `SHORT: max(mark,
oracle)` — and the engine **refuses to liquidate when the oracle is stale**
(`lib.rs liquidate_position_v2` dual-source gate + staleness gate). A fresh
oracle move can thus tip a position underwater *without* waiting for the mark to
catch up, **and** a stale oracle can never wrongfully liquidate (liquidation is
paused in exactly that state). Hardened (H5) so the EMA mark is clamped into the
oracle band before the per-fill move clamp — the mark cannot cumulatively drift
outside the oracle band and feed a wrongful worse-of decision (`lib.rs apply_fill`
band clamp; the band-clamp and liquidation staleness gates are byte-identical, so
there is no state where the band clamp is off *and* a wrongful liquidation can
fire).

**Novelty / non-obviousness.** No surveyed on-chain DEX runs a dual-source
worse-of health check; the non-obvious combination is *both* sources, *adverse*
selection, *plus* a staleness refusal that is provably consistent with the
mark-band clamp.

**Verification status.** The band-clamp / staleness-gate consistency was
adversarially verified (2025 final battle-test): no combined state exists where
the clamp is disabled and a wrongful liquidation fires.

---

## Invention 3 — Permissionless pre-committed JIT-liquidation auction

**Field / problem.** When a position is liquidated, the close price determines how
much the trader loses and how much the insurance fund must absorb. A naive
synthetic close at `oracle ± penalty` is worst-case for the trader.

**The mechanism.** Market makers **pre-commit** liquidation close-price offers on
chain (`place_jit_liquidation_offer`, `state_v3::JitLiquidationOfferAccount`). At
liquidation, the engine **walks the pre-committed offers** in `remaining_accounts`
and uses the **best price that beats the synthetic `oracle ± penalty`** for the
trader's direction, decrementing the consumed offer
(`lib.rs liquidate_position_v2` JIT auction). The trader loses *less*, the
insurance fund draws *less*, and the maker earns a guaranteed fill at a price it
chose — a permissionless, pre-committed liquidation-price-improvement primitive.

**Novelty / non-obviousness.** No surveyed on-chain DEX has a permissionless
pre-committed JIT-liquidation auction (Hyperliquid keeps liquidations private;
Drift/dYdX route through keepers + insurance). The non-obvious step is letting any
maker pre-commit a tighter close price that strictly improves the *trader's*
outcome.

---

## Invention 4 — Conservation-proven settlement layer (nonce + bad-debt waterfall + residual credit)

**Field / problem.** A perp that splits *matching* (book mutation) from *economic
settlement* (position/collateral updates) must guarantee each settlement (a)
cannot be replayed and (b) conserves value even when a position closes bankrupt.

**The mechanism (three coupled parts, all recently hardened + proven).**
1. **Monotonic settlement nonce (H1).** Each settlement carries a `fill_seq` that
   must *strictly exceed* a per-market `last_settlement_seq`, which it then
   advances — atomically with the transaction. A replayed or out-of-order
   settlement (a crashed/restarting sequencer re-emitting a batch) is rejected
   on-chain (`lib.rs apply_fill / apply_flp_fill`; `MarketAccount.last_settlement_seq`).
2. **Bad-debt waterfall (H6).** A loss exceeding a position's collateral
   saturates the bucket to 0 and the deficit is **absorbed by the insurance fund**
   (pure ledger reconciliation — no token transfer, the shared vault already holds
   the funds) instead of reverting settlement and stranding the position
   (`lib.rs cover_bad_debt`).
3. **Residual credit on losses (H7).** The collateral actually removed by a loss
   credits the haircut `Residual` (Invention 1) — and is *always ≤ the loss*, so
   the Residual can never be over-credited.

**Verification status.** The conservation core is **machine-proven in Kani**
(exhaustive over all 2⁶⁴ inputs): `cross_loss_shortfall` conserves the pool and
**never over-credits** (`removed ≤ debit` → `h` can never be inflated → no value
mint), and `compute_realized_pnl_routing` debits/credits exactly the routed
bucket and never the other (`lib.rs #[cfg(kani)] mod h6_h7_solvency_proofs`;
`VERIFICATION: SUCCESSFUL`).

**Novelty / non-obviousness.** The novel combination is a replay-nonce settlement
*plus* a fail-forward bad-debt waterfall *plus* a residual-conservation credit,
*proven together* to neither mint nor burn value.

---

## Invention 5 — Stress-lattice portfolio margin over a hypertree CLOB with ER acceleration

**Field / problem.** Cross-margin engines that net positions can under-margin
correlated/hedged portfolios; on-chain order books are CU-expensive; L1 latency is
~400ms.

**The mechanism.** (a) Portfolio margin is assessed across a **scenario lattice**
(`matcher::risk::assess_margin` / `assess_margin_unified`) with maintenance margin
composed of **tiered + OI-scaled + concentration** terms, unifying cross and
isolated positions in one stress evaluation. (b) The order book is a single-account
**hypertree** (overlapping red-black trees) with strict **price-time priority**
and zero-copy access, expandable in place to ~10k nodes. (c) Matching/settlement
can run on a **MagicBlock Ephemeral Rollup** (delegate → execute sub-50ms →
commit/undelegate) with an L1 fallback (`er.rs`).

**Novelty / non-obviousness.** The combination — stress-lattice *portfolio* margin
*with the specific MMR composition*, over a *price-time* hypertree (vs. Manifest's
price-only ordering), with an ER acceleration path and L1 fallback — is, per the
surveyed field, not present in any single competing system. Measured CU on the
Pinocchio port (`apply_fill` 1,469 CU vs. 37,779 Anchor, −96%) is itself a
competitive datum.

---

## Cross-cutting: verifiability as a feature

The protocol's *verifiability* is itself a differentiator: solvency-critical math
is **machine-checked** (Lean at the real divisor + Kani exhaustive proofs), and
every protocol change in the security-hardening effort was **adversarially
re-verified** (156+ distinct attacks attempted and survived across boundary,
archetype, historical, novel, combined-vector, and economic/architectural passes).
The honest claim this supports is **"no known attack succeeds + the solvency core
is machine-proven + every change adversarially verified"** — not the unprovable
"unhackable."

---

*All mechanism descriptions cite real source. Verification claims are reproducible
(`cargo kani --package flash-book --features no-entrypoint`; Lean via the QEDGen
pipeline — see `docs/FORMAL_VERIFICATION.md`). No individual is named.*
