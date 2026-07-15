# Critical-path 6+2+2 devnet acceptance — findings

Launch-gate acceptance of the reconciled margin-gate / liquidation-lock / withdrawal-price fix queue, run **live on a fresh
throwaway devnet program** built from the reconciled HEAD. Harness:
`er-acceptance/critical_path_acceptance.mjs` (self-contained genesis + drive; re-runnable).
Machine-readable results + Explorer links: `er-acceptance/critical_path_results.json`.

## Provenance (Stages 1–2)

| | |
|---|---|
| Reconciled branch | `sec/critical-path-native` (margin-gate/liquidation-lock/withdrawal-price cherry-picked onto current main; A2 cores untouched) |
| In-tree gates | build-sbf 0-warn · **631 tests / 0 fail** · clippy `-D warnings` clean · fmt clean · Kani 73 |
| Artifact | `clober.so` built fresh with `declare_id!` = the throwaway id |
| Deployed program (throwaway) | `BRtnEAZ6Tc61gz8m93unL1vzaC4GjtHViLCU8JqKB2gD` (devnet; not main's `5VqBgu…`) |
| Deployed-hash verify | on-chain bytes sha256 == artifact sha256 `326896b0fc85fafe0f974383a678a64edee682885d3258296016d17e8904f2b0` ✓ |

Genesis is built from scratch on the fresh program each run: fresh quote mint (singleton-reused
across runs), insurance fund (+ vault), LP exposure, an IM>0 market (params cloned from the old
program's reference market), book, and armed fill-commitment ring. Real positions form via the
full match→settle loop (maker rests · taker crosses · sequencer `apply_fill`) — the sequencer
defaults to the deployer, so settlement is driven first-party with a keccak-matched ring pop.

## Result table (8 PASS · 0 FAIL · 5 UNDRIVEN)

| ID | Group | Verdict | What ran on-chain |
|---|---|---|---|
| margin-gate-3 | margin-gate | **PASS** | `place_iceberg_order` opening from a 0-collateral state → rejected **InsufficientCollateral (7204)** |
| margin-gate-5 | margin-gate | **PASS** | `place_bracket_order` opening from a 0-collateral state → rejected **InsufficientCollateral (7204)** |
| margin-gate-6 | margin-gate | **PASS** | `vault_place_order` from a 0-collateral vault → rejected **InsufficientCollateral (7204)** |
| HA-RO | margin-gate | **PASS** | reduce-only inject is EXEMPT from the intake gate → accepted (real sig) |
| margin-gate-1 | margin-gate | UNDRIVEN | `execute_trigger_order` — same `gate_injection_open` helper; requires an existing position (funded path), not drivable from the 0-collateral trader |
| margin-gate-2 | margin-gate | UNDRIVEN | `execute_twap_slice` — same helper; twap slice-eligibility (`OutOfRange`) precedes the gate on a fresh market |
| margin-gate-4 | margin-gate | UNDRIVEN | `replenish_iceberg` — same helper; needs a placed-then-depleted iceberg |
| POS | setup | **PASS** | real position formed via `apply_fill` (taker long 1 @ 100000), settled on L1 |
| LIQ | setup | **PASS** | real `liquidate_position` injects the synthetic close (`order_type==3`) after an adverse oracle move |
| liquidation-lock-1 | liquidation-lock | **PASS** | liquidatee's `cancel_order` on their own `order_type==3` → rejected **LiquidationOrderNotCancelable (8325)** — the dodge is blocked |
| liquidation-lock-2 | liquidation-lock | **PASS** | market authority `retire_liquidation_order` on the stranded `order_type==3` → accepted (real sig) |
| withdrawal-price-1 | withdrawal-price | UNDRIVEN | benign-mark withdraw positive control — see note |
| withdrawal-price-2 | withdrawal-price | UNDRIVEN | worse-of-mark withdraw rejection — see note |

## Honest notes

- **margin-gate gate (confirmed HIGH) — proven live.** All 6 opening injection/vault paths route through
  one verified helper (`gate_injection_open → assert_injection_intake`; 6 call sites confirmed in
  source). The helper is proven on-chain on **3 structurally-independent paths** (iceberg place,
  bracket place, vault place) all rejecting with the exact `InsufficientCollateral`, plus the
  reduce-only carve-out accepting. margin-gate-1/2/4 exercise the *same* helper but their upstream
  condition/timing plumbing (existing position, slice eligibility, placed iceberg) can't be
  satisfied from a zero-collateral trader on a fresh market — reported UNDRIVEN, never faked.
- **liquidation-lock lock (confirmed HIGH) — proven live end-to-end.** The `order_type==3` order was produced
  by a **real liquidation** (never hand-crafted): a real taker position was opened, the oracle was
  stepped down under the envelope cap until the worse-of health went liquidatable, and
  `liquidate_position` injected the close. Its id was reconstructed from the emitted
  `LiquidationInjectedEvent` (the event's `side` is the *position* side; the order rests on the
  opposite close side). The owner's cancel was rejected with the right error; the authority's
  retirement succeeded.
- **withdrawal-price (MED) — not cleanly demonstrable on these params.** The clean accept→reject withdraw flip
  requires a collateral window `withdraw-margin < C < max-loss`. On the cloned (mainnet-like) params
  the withdraw's full-portfolio stress margin is ≈ the position's max loss, so a thin trader has no
  free collateral at the benign mark (benign withdraw rejects at 40k, 90k, and 120k) — there is no
  window in which a price move flips an *accepted* withdraw to rejected. This is a genuine property
  of the stress-lattice margin, reported as UNDRIVEN rather than claimed. withdrawal-price remains covered by the
  in-tree test suite and the reconciled source (`effective_health_mark` worse-of routing).

## Sample real signatures (devnet Explorer)

- Program deploy: `VToqywExq88bEWjde8NySgNUGbMjBh4bxhW95bL5dCxLbxBjVvcEQ3DgyBjtpJVWobdcLzBHSLrUtcWsuzZrbJR`
- LIQ (real liquidation → order_type==3): `5u89tRExDcDiaKSf2jVyR7zovguyKRZboVGtuNdLAQfKPm8iKUd9Y74QjKnoJfq2xk4dREXXwGskpZwY6DYQeAWR`
- liquidation-lock-2 (authority retires the liquidation order): `5UFhxWLsqEHDkjb5fK1eZXxwfJkmdj8LvzDWVcFUvUjze62ruKCPj6TfXp24a5piUjK3KLprWBhgS3Wuo1QvqKZE`

(The margin-gate / liquidation-lock-1 negative rows are real on-chain rejections carrying the asserted error codes; the
full per-row set with Explorer links is in `critical_path_results.json`.)
