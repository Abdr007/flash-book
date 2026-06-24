# Flash Book — Technical Differentiators

> What is technically distinctive about Flash Book, grounded in the actual
> implementation and (where stated) machine-checked. **Verified-only:** every
> mechanism is cited to real source (`file:line`); every verification claim is
> reproducible (`cargo kani …`, Lean via the QEDGen pipeline). Where a mechanism
> is a *design target* rather than something already measured/proven, it is
> labeled as such. Nothing here is marketing.

**System context.** Flash Book is a fully on-chain central-limit-order-book
(CLOB) perpetual-futures DEX on Solana: a red-black "hypertree" order book with
price-time priority, a stress-lattice portfolio-margin risk engine, and optional
sub-50ms execution via MagicBlock Ephemeral Rollups. Program:
`programs/flash-book/src`.

---

## 1. Formally-proven junior-claim PnL-conservation engine ("H-Haircut")

**Problem.** In a perp venue, realized *profits* are paid from a shared pool that
also backs losing positions and liquidity. If the pool is depleted by bad debt (a
position gapping past its collateral), paying winners their full realized PnL makes
the protocol **insolvent**. Most designs socialize this *reactively* (ADL,
insurance draws) after insolvency is already possible.

**The mechanism.** Released positive PnL is a **junior claim**, not immediately
spendable collateral:
1. It is parked in a per-position **reserve** and matures over a warm-up window
   (`matcher::haircut::apply_release`; `state_v3::PositionHaircutStateAccount`).
2. Matured PnL converts to collateral only via `convert_position`, at a **solvency
   factor** `h = min(Residual, MaturedTotal) / MaturedTotal`, where
   `Residual = V − C_tot − I` (protocol assets − committed collateral − insurance)
   (`matcher::haircut::compute_h`; `lib.rs convert_position`).
3. `Residual` is **delta-tracked** by every money-moving instruction — funding
   (`settle_funding`), realized losses (`accrue_haircut_loss`, H7), and conversions.

So the protocol **cannot pay out more realized gains than its actual backing** —
bad debt is absorbed by haircutting junior claims (`h < 1`), preventing insolvency
structurally rather than reacting to it.

**What makes it different.** No surveyed on-chain perp (Drift, GMX, dYdX,
Hyperliquid, or the surveyed reference implementations) gates released PnL by a
delta-tracked solvency factor.

**Verification — strongest.** The conservation/solvency bound (`credit ≤ Residual`,
`credit + dust == matured`, `h ≤ 1`) is **machine-proven in Lean 4 at the real 1e9
divisor** (where CBMC/Kani SAT backends are incomplete on 128-bit non-power-of-two
division) + **5 Kani proofs** (`matcher/haircut.rs`; `docs/FORMAL_VERIFICATION.md`).
*No surveyed perp has a formally-proven PnL-conservation mechanism.*

---

## 2. Dual-source "worse-of" liquidation gate with staleness refusal

**Problem.** Using the **mark** alone (an EMA of fills) lets a thin tape lag a real
move; using the **oracle** alone trusts a single feed that can gap or be
manipulated (Mango / JELLY class). Most perps pick one.

**The mechanism.** Health is evaluated at the **worse of the two sources** for the
position's direction — `LONG: min(mark, oracle)`, `SHORT: max(mark, oracle)` — and
the engine **refuses to liquidate when the oracle is stale**
(`lib.rs liquidate_position_v2`). A fresh oracle move can tip a position underwater
without waiting for the mark; a stale oracle can never wrongfully liquidate
(liquidation is paused in exactly that state). Hardened (H5) so the EMA mark is
clamped into the oracle band *before* the per-fill move clamp — the mark cannot
cumulatively drift outside the band and feed a wrongful worse-of decision; the
band-clamp and liquidation staleness gates are byte-identical, so there is no state
where the clamp is off *and* a wrongful liquidation can fire.

**What makes it different.** No surveyed on-chain DEX runs a dual-source worse-of
health check with a provably-consistent staleness refusal.

**Verification.** The clamp/staleness consistency was adversarially verified: no
combined state exists where the clamp is disabled and a wrongful liquidation fires.

---

## 3. Permissionless pre-committed JIT-liquidation auction

**Problem.** A liquidation's close price sets how much the trader loses and how
much the insurance fund absorbs. A naive synthetic close at `oracle ± penalty` is
worst-case for the trader.

**The mechanism.** Makers **pre-commit** close-price offers on chain
(`place_jit_liquidation_offer`, `state_v3::JitLiquidationOfferAccount`). At
liquidation the engine walks the offers and uses the **best price beating the
synthetic `oracle ± penalty`** for the trader's direction
(`lib.rs liquidate_position_v2`). The trader loses *less*, insurance draws *less*,
and the maker earns a guaranteed fill — a permissionless, pre-committed
liquidation-price-improvement primitive.

**What makes it different.** No surveyed on-chain DEX has a permissionless
pre-committed JIT-liquidation auction (Hyperliquid keeps liquidations private;
Drift/dYdX route through keepers + insurance).

---

## 4. Conservation-proven settlement layer (nonce + bad-debt waterfall + residual credit)

**Problem.** A perp that splits *matching* (book mutation) from *economic
settlement* must guarantee each settlement (a) cannot be replayed and (b) conserves
value even when a position closes bankrupt.

**The mechanism (three coupled parts, recently hardened + proven).**
1. **Monotonic settlement nonce (H1).** Each settlement carries a `fill_seq` that
   must strictly exceed a per-market `last_settlement_seq`, then advances it —
   atomically with the tx. A replayed/out-of-order settlement (a crashed/restarting
   sequencer re-emitting a batch) is rejected on-chain.
2. **Bad-debt waterfall (H6).** A loss exceeding a position's collateral saturates
   the bucket to 0 and the deficit is **absorbed by the insurance fund** (pure
   ledger reconciliation — no token transfer) instead of reverting settlement and
   stranding the position (`lib.rs cover_bad_debt`).
3. **Residual credit on losses (H7).** The collateral actually removed by a loss
   credits the haircut `Residual` — always ≤ the loss, so it can never be
   over-credited.

**Verification.** The conservation core is **machine-proven in Kani** (exhaustive
over all 2⁶⁴ inputs): `cross_loss_shortfall` conserves the pool and never
over-credits (`removed ≤ debit` → `h` never inflated → no value mint), and
`compute_realized_pnl_routing` debits/credits exactly the routed bucket and never
the other (`lib.rs #[cfg(kani)] mod h6_h7_solvency_proofs` — `VERIFICATION:
SUCCESSFUL`).

---

## 5. Stress-lattice portfolio margin over a price-time hypertree with ER acceleration

**Problem.** Naive cross-margin can under-margin hedged portfolios; on-chain books
are CU-expensive; L1 latency is ~400ms.

**The mechanism.** (a) Portfolio margin is assessed across a **scenario lattice**
(`matcher::risk::assess_margin` / `assess_margin_unified`) with maintenance margin
composed of **tiered + OI-scaled + concentration** terms, unifying cross and
isolated positions. (b) The book is a single-account **hypertree** (overlapping
red-black trees) with strict **price-time priority** and zero-copy access,
expandable in place to ~10k nodes. (c) Matching/settlement can run on a **MagicBlock
Ephemeral Rollup** with an L1 fallback (`er.rs`).

**What makes it different.** The combination — stress-lattice *portfolio* margin
with that MMR composition, over a *price-time* hypertree (vs. Manifest's price-only
ordering), with an ER acceleration path and L1 fallback — is not present in any
single surveyed competitor. Measured CU on the Pinocchio port (`apply_fill` 1,469
CU vs. 37,779 Anchor, −96%) is a competitive datum.

---

## Cross-cutting: verifiability as a feature

Solvency-critical math is **machine-checked** (Lean at the real divisor + Kani
exhaustive proofs), and every change in the security-hardening effort was
**adversarially re-verified** (156+ distinct attacks attempted and survived across
boundary, archetype, historical, novel, combined-vector, and economic passes). The
honest claim this supports is **"no known attack succeeds + the solvency core is
machine-proven + every change adversarially verified"** — not the unprovable
"unhackable."

---

*All mechanism descriptions cite real source; verification claims are reproducible
(`cargo kani --package flash-book --features no-entrypoint`; Lean via the QEDGen
pipeline — see `docs/FORMAL_VERIFICATION.md`). No individual is named.*
