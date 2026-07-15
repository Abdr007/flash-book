# 3.1 — Per-source-domain realizable credit (design)

**Status: DESIGN + core-math verified; engine-wiring is the tracked remainder.**
This document specifies the mechanism and its safety invariant. The core credit
arithmetic (`usable = pnl · rate_bps / 10_000` with `rate_bps ≤ 10_000`) was
**verified with Kani locally — VERIFICATION SUCCESSFUL, 0 of 5 checks failed**:
usable credit never exceeds the paper PnL, and `rate_bps == 0 ⇒ usable == 0`.
Because that check involves a wide-integer multiply+divide that CBMC only
discharges in ~300–700s, it is **not committed as a CI-resident Kani harness**
(it would bloat the gate); the durable CI proof belongs in **Lean** — the same
128-bit route as the haircut theorem — including the `rate = min(1,
backing/claims)` step whose symbolic divide CBMC cannot handle at all. That Lean
theorem + the engine-wiring + devnet deploy + live-re-verify are the tracked
3.1 remainder. **No production path uses it yet — deliberately, so there is no
dormant half-wired money code.**

## The attack it closes

An oracle-pump attack: manipulate a thin
or stale market's price so a position shows large *paper* PnL, then use that
paper PnL as if it were real value — to back margin on another position, cure a
loss, or withdraw. A single global haircut ratio and a price-scenario lattice
both miss this because they don't ask *can the opposing side of THIS market
actually pay?*

## The mechanism

For each market (domain), a profitable leg's usable PnL is capped by that
market's own ability to pay:

```
credit_rate(market) = min(1, backing(market) / claims(market))
usable_pnl(leg)     = pnl(leg) * credit_rate(market of leg)
```

- `claims(market)` = total positive PnL owed to the winning side of that market.
- `backing(market)` = the value the losing side of that market can actually
  deliver (their collateral + the market's realized/settled backing), i.e. what
  is genuinely collectable, not what the mark implies.

A manipulated / thin / stale market has `backing << claims`, so `credit_rate →
0`, so `usable_pnl → 0`: **the paper profit cannot back margin, cure a loss, or
be withdrawn.** Only the fraction the opposing side can truly pay is ever usable.

## Safety invariant (the marquee property)

> A leg's usable PnL never exceeds the opposing side's ability to pay:
> `usable_pnl ≤ pnl` and `usable_pnl ≤ pnl · backing / claims`. In particular
> `backing == 0 ⇒ usable_pnl == 0` — a market with no real backing mints zero
> usable credit from any amount of paper PnL.

This composes with the two already-proven halves of the launch claim:
- **3.2** (`withdraw_cannot_self_liquidate_below_maintenance`) — can't withdraw
  into liquidation.
- **5.2** (`bad_debt_coverage_is_insurance_isolated_and_bounded`) — an LP loss
  can't drain insurance.
- **3.1** (this) — a manipulated market can't mint cross-domain credit.

Together they make *both halves* of "the HL-$20M attack is impossible" machine-checked.

## Where it hooks (engine-wiring remainder — the multi-week part)

`usable_pnl` (not raw mark PnL) must be the value fed to:
1. **`assess_margin`** — equity uses usable PnL, so paper profit can't satisfy IM/MM.
2. **the withdraw gate** — withdrawable is computed from usable equity.
3. **loss cure / haircut routing** — a loss can only be cured by usable credit.

Each site is settlement-adjacent: the change must be differential-confirmed
byte-identical on the non-manipulated path (backing ≥ claims ⇒ credit_rate = 1 ⇒
usable_pnl == pnl, so honest markets are unaffected), CU-measured, IDL-regen'd,
deployed, and live-re-verified — and the acceptance suite (reduce-only, funding,
reconciler) re-run to confirm no regression. That is the tracked 3.1 build.

## Honesty line

Until the wiring lands and is live-verified, the public claim stays the honest
half: self-liquidation and insurance-drain proven impossible; the
oracle-manipulation proof (this) is designed and its core math is proven, with
engine integration in progress.
