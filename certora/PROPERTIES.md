# Clober — Formal Property Specification

This is the **formal property set** for Clober: the invariants the protocol
must satisfy, stated precisely enough to (a) machine-check today with Kani / Lean
and (b) encode for the **Certora Prover for Solana** (the production bar set by
Manifest and Kamino).

**Status legend.**
- `[KANI]` — machine-proven exhaustively today (`cargo kani --package clober
  --features no-entrypoint`), see `programs/clober/src/**/*.rs` `#[cfg(kani)]`.
- `[LEAN]` — machine-proven over unbounded `Nat`/`Int` at the real divisor
  (`formal_verification/lean/`), `#print axioms`-clean.
- `[CERTORA-TARGET]` — stated here; to be discharged by the Certora Prover
  (requires a Certora license — not run in this environment).
- `[REQUIRE]` — currently enforced by an on-chain `require!`/constraint (runtime,
  not yet a proof).

> **Honesty.** Nothing here claims to be Certora-verified. The `[KANI]`/`[LEAN]`
> rows are reproducible today; the `[CERTORA-TARGET]` rows are the specification
> to run when a license is available.

---

## 1. Solvency & value conservation (the core)

**P-SOLV-1 — No value mint on loss settlement.** For every realized-loss
settlement, the collateral removed equals the loss covered, and the haircut
Residual is credited by at most that amount.
`removed + shortfall == loss ∧ removed ≤ loss` (so `h` is never inflated).
→ **`[KANI]`** `cross_loss_shortfall_conserves_and_never_overcredits`.

**P-SOLV-2 — Exact, one-sided PnL routing.** A gain credits exactly the routed
collateral bucket by exactly the gain and touches no other bucket; a loss only
shrinks the routed bucket and never debits more than the loss.
→ **`[KANI]`** `realized_pnl_routing_{gain,loss}_*`.

**P-SOLV-3 — Haircut conversion never exceeds backing.** A `convert_position`
credit satisfies `credit ≤ Residual` and `credit + dust == matured` at the real
`1e9` divisor.
→ **`[LEAN]`** `Haircut.convert_ensures_0`, `Haircut.solvency_single_convert`
(+ `[KANI]` at a power-of-two divisor).

**P-SOLV-4 — Global solvency invariant.** At all times
`vault.amount ≥ Σ trader_collateral + Σ LP_capital + insurance.balance`
(no instruction may make protocol liabilities exceed protocol assets).
→ **`[KANI]`** (protocol-owned buckets) `assess_solvency` (`matcher::insurance`) —
`solvent_iff_vault_covers_buckets` + `surplus_exact_when_solvent` (vault accounts
exactly to insurance + LP + surplus; no value invented); `verify_protocol_solvency`
routes through it.
→ **`[KANI]`** (FULL invariant, incl. trader collateral) `assess_solvency_full` —
`full_solvent_iff_vault_covers_all_liabilities` proves `solvent ⇔ vault ≥
Σ collateral + LP + insurance` with exact surplus. The runtime check is the
DRIFT-FREE one-sided sweep `partial_collateral_proves_insolvent` (proven sound by
`partial_insolvency_detector_is_sound`: it fires only on genuine insolvency, for
any real total ≥ the summed subset); the permissionless `verify_collateral_solvency`
instruction sums REAL collateral from deduplicated trader-state / isolated-position
accounts and routes through it — so it cannot desync from the 47 collateral-mutation
sites the way a stored aggregate would.
→ **`[CERTORA-TARGET]`** the broader `vault ≥ Σ trader_collateral + LP + insurance`
*preserved by every instruction* (the Manifest "loss-of-funds" set) — the all-paths
proof the one-sided runtime sweep cannot give. Scaffolded in `certora/specs/solvency.spec`
+ `certora/solana_solvency.conf`; runnable once a Certora Solana license is wired.

**P-SOLV-5 — Residual conservation identity.** `Residual == V − C_tot − I` is
preserved by every money-moving instruction (funding pay/receive, realized
loss/gain, convert, dust flush).
→ **`[KANI]`** (delta-application core) `apply_residual_delta` (`matcher::haircut`) —
`residual_delta_applied_exactly` + `residual_delta_roundtrip_conserves` (exact +
perfectly invertible; the tracking never drifts). **`[CERTORA-TARGET]`** the
identity *preserved across all money-moving instructions* (whole-program).

---

## 2. Funding

**P-FUND-1 — Funding is zero-sum.** For equal notional and index pair, a long
owes exactly what a short receives: `funding_owed(long) + funding_owed(short) ==
0`. Funding moves value between sides; it cannot mint or burn it.
→ **`[LEAN]`** `Funding.funding_zero_sum`. (Proven over unbounded `Int`, not
Kani: CBMC must bit-blast the 128-bit `notional · delta` multiply and does not
terminate — the same SAT limit as the haircut. The result holds independently of
the `>> 64` rounding, by exact sign cancellation.)

**P-FUND-2 — No accrual from a static index.** `cum_now == cum_entry ⇒ owed == 0`.
→ **`[LEAN]`** `Funding.funding_zero_when_no_index_move`.

---

## 3. Margin

**P-MARGIN-1 — Maintenance margin never below the floor (INV-M4).** The effective
MMR is never below `base_mmr_bps`; the OI/concentration surcharge only adds.
→ **`[KANI]`** `effective_mmr_never_below_base_floor`.

**P-MARGIN-2 — Surcharge bounded by its cap.** The OI-scaled surcharge never
exceeds `oi_max_extra_bps`; `slope == 0` disables it.
→ **`[KANI]`** `oi_scaled_never_exceeds_cap`, `oi_scaled_zero_slope_disables`;
also **`[LEAN]`** `OiMmr.oiScaled_le_cap`.

**P-MARGIN-3 — Surcharge monotone in open interest** (the property CBMC can't
decide at `/1e6`). A larger crowded book is never under-margined.
→ **`[LEAN]`** `OiMmr.oiScaled_mono`.

**P-MARGIN-4 — Margin-walk completeness.** Every margin gate that walks
`remaining_accounts` evaluates the trader's *complete, authentic, distinct, live*
position set; omitting/substituting/duplicating a position cannot lower required
margin.
→ **`[REQUIRE]`** `verify_position_pda` + exact-count + dedupe + `size>0` on
`partial_withdraw_collateral` / `sweep_collateral` / `liquidate_portfolio_v2`;
**`[CERTORA-TARGET]`** prove the walk is exhaustive vs. the trader's on-chain
position set.

---

## 4. Settlement integrity

**P-SETTLE-1 — No replay.** Each settlement carries a `fill_seq` strictly greater
than `market.last_settlement_seq`, which it then advances atomically; a replayed
or out-of-order settlement reverts the whole transaction.
→ **`[KANI]`** `advance_settlement_seq` (`matcher::fill_commitment`) —
`nonce_rejects_non_increasing`, `nonce_advance_is_strict_and_exact`,
`nonce_chain_strictly_monotone`. Both `apply_fill` and `apply_lp_fill` route
through the proven helper.

**P-SETTLE-2 — Bad debt is bounded.** A bankrupt close draws at most `shortfall`
from the insurance fund (saturating at its balance) and never reverts settlement.
→ **`[KANI]`** (P-SOLV-1) + **`[REQUIRE]`** `cover_bad_debt`.

**P-SETTLE-3 — Settlement authority.** Only the market's configured `sequencer`
may post fills (fail-closed on the zero pubkey).
→ **`[REQUIRE]`** C-1 `require_keys_eq!`; **`[CERTORA-TARGET]`** prove no
state-mutating settlement path bypasses it.

---

## 5. Liquidation

**P-LIQ-1 — No wrongful liquidation.** A position healthy at the fresh oracle is
never liquidated: the health price is the worse of (mark, oracle), liquidation is
refused when the oracle is stale, and the EMA mark is clamped into the oracle band.
→ **`[KANI]`** worse-of core: `worse_of_health_price` (`matcher::liquidation`) —
`health_price_worse_for_{long,short}` (always the worse of the two real sources,
never under-states risk), `health_price_is_a_real_source` (never a fabricated
price); `liquidate_position_v2` routes through it. **`[REQUIRE]`** staleness gate +
band clamp (byte-identical, no gap). **`[CERTORA-TARGET]`** the fully-combined gate.

**P-LIQ-2 — No duplicate liquidation.** No second synthetic close order is
injected (and no second reward paid) while one for the same position rests.
→ **`[REQUIRE]`** book scan in `liquidate_position_v2` *and* `liquidate_portfolio_v2`.

---

## 6. Matching engine

**P-MATCH-1 — Price-time priority.** The hypertree walks orders ascending by
`order_id`, so the encoding alone must yield correct price-time priority on both
books: a better price always sorts first (asks ascending, bids descending), and at
equal price the earlier `seq` sorts first (FIFO, both sides).
→ **`[KANI]`** `encode_order_id` (`state_v2`) — `ask_lower_price_fills_first`,
`bid_higher_price_fills_first`, `earlier_seq_fills_first_at_same_price` (rules out
the old LIFO-bid bug). Inputs in the encodable range (placement-enforced).

**P-MATCH-2 — Order-id injectivity.** No two live orders with distinct
`(price, seq)` collide on `order_id` (the RBT key is injective, so no order can
displace another).
→ **`[KANI]`** `distinct_orders_never_collide`.

---

## Running

```bash
# Kani (today):
cargo kani --package clober --features no-entrypoint
# Lean (today):
cd formal_verification/lean && lake build
# Certora (requires a Certora license; encode the [CERTORA-TARGET] rows):
#   certoraRun ... --verify Clober:certora/specs/<spec>.spec
```

*No individual is named in this document. Every `[KANI]`/`[LEAN]` row is
reproducible with the commands above.*
