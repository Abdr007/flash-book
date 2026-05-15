# Auditor Handoff Brief

For external audit teams engaging Flash Book (Sherlock contest, Trail
of Bits engagement, Certora formal verification, etc.).

## Codebase snapshot

- **Anchor program**: `programs/flash-book/src/` (~14k lines Rust)
- **Pure-math modules**: `programs/flash-book/src/matcher/` (45+ files, ~7k lines)
- **TypeScript SDK**: `sdk-ts/src/` (~2k lines)
- **Parity ports**: `src/` (~3k lines TypeScript)
- **Tests**: 571 Rust + 236 TypeScript = **807 total**

## Pre-audit internal review

See [`AUDIT.md`](AUDIT.md) for the 19 internal audits run:

| Category | Audits | Findings |
|---|---|---|
| Structural (spaces, seeds, codes) | 1–3, 5 | 0 |
| Math correctness | 4, 6, 10, 12, 13 | 0 |
| Authority + accounts | 8 | 0 |
| Oracle safety | 9 | 2 HIGH (fixed) |
| Parameter bounds | 11 | 1 MEDIUM (fixed) |
| Hot paths | 7, 14, 15 | 0 |
| Quality markers | 16, 17, 18, 19 | 0 |

**3 findings total. All fixed. Final state: 571 tests passing, clean
build.**

## Recommended audit scope

### Tier 1 — Critical (must audit)

| Surface | LOC | Risk |
|---|---|---|
| `lib.rs::apply_fill` (~600 lines) | 600 | Position state corruption, fee mis-routing |
| `lib.rs::apply_flp_fill` (~300 lines) | 300 | FLP-side exposure tracking |
| `lib.rs::liquidate_position_v2` (~400 lines) | 400 | Wrongful liquidation, reward exfil |
| `lib.rs::auto_deleverage` (~200 lines) | 200 | ADL selection, math correctness |
| `lib.rs::settle_funding` | 100 | Funding overflow, idempotency |
| `lib.rs::run_batch_v2` (matcher walk) | 500 | Order matching, self-trade prevention |
| `lib.rs::place_taker_order_v2` | 300 | Pre-fill validation, OI caps |
| `matcher/risk.rs::assess_margin*` | 600 | Stress lattice correctness |
| `matcher/haircut.rs` (Wave 24) | 700 | H-invariant solvency proof |
| `matcher/envelope.rs` (Wave 26) | 300 | Envelope inequality proof |

### Tier 2 — Standard (audit if budget permits)

| Surface | LOC | Risk |
|---|---|---|
| `lib.rs::update_market_params` | 100 | Parameter validation completeness |
| `lib.rs::withdraw_collateral` | 100 | SPL CPI atomicity |
| `lib.rs::deposit_flp_capital` | 150 | LP share dilution |
| `lib.rs::withdraw_flp_capital` | 200 | LP solvency math + position-aware NAV |
| All Wave 24-65 pure math modules | 4k | Borrow fee, funding velocity, ARG, etc. |

### Tier 3 — Lower priority

- Trigger / TWAP / iceberg / bracket order ix
- Sub-account routing (Phase 2c-2f)
- ER delegation surface (`er.rs`)
- Hypertree implementation (vendored, GPL-3.0)

## Known design tradeoffs

These are intentional and should not be flagged as bugs:

1. **Permissionless `settle_funding`** — anyone can crank. By design;
   the position's identity is bound by PDA seeds, not the caller.
2. **Permissionless `liquidate_position_v2`** — anyone can liquidate.
   By design (Hyperliquid pattern). Cooldown + oracle staleness
   protect the trader; competition among liquidators tightens spreads.
3. **Mark price lags oracle by 1-2 fills** — by design. EMA blend
   smooths the mark; `settle_mark` resets when drift exceeds clamp.
4. **No margin check at `place_limit_order_v2`** — by design. The
   order rests on the book without moving money; margin check happens
   at fill time via `apply_fill`'s pre-snapshot of positions.
5. **Saturating arithmetic on isolated bucket losses** — by design.
   Documented in `lib.rs:2667-2680`. Catches in next health check
   rather than reverting (would prevent settle).

## Provability surface

### Pure-math modules (no Solana types)

These are no_std-compatible and suitable for **Kani formal
verification**:

- `matcher/haircut.rs` (H invariant)
- `matcher/envelope.rs` (per-slot envelope)
- `matcher/side_accrual.rs` (A/K/F/B math)
- `matcher/risk.rs::*` (stress lattice, tiered MMR)
- `matcher/funding.rs` (cumulative index)
- `matcher/liquidation.rs` (close-side math)
- `matcher/arg.rs` (sandwich tax)
- `matcher/cross_margin_weights.rs` (joint margin sqrt)
- 15+ more

### Existing property tests

- `proptest_haircut.rs` — 10 properties × 2000 cases (H solvency)
- `proptest_envelope.rs` — 7 properties (gate monotonicity)
- `proptest_arg.rs` — 4 properties (sandwich tax invariants)
- `proptest_cross_margin.rs` — 8 properties (joint margin bounds)
- `proptest_isolated.rs` — 6 properties × 2000 cases (bucket independence)
- `proptest_liquidation.rs` — 6 properties (close ordering)
- `proptest_risk.rs` — 7 properties (margin math)
- `proptest_modules.rs` — 14 properties (mixed)
- `proptest_new_features.rs` — 19 properties (mixed)

Total: 91 property tests, each running 2000 random cases per CI run.

## Suggested audit deliverables

1. **Findings report** with severity (Critical / High / Medium / Low /
   Informational) and recommended fixes.
2. **Independent property tests** for any invariant the audit team
   identifies as load-bearing.
3. **Manual review of the H-invariant proof** in `HAIRCUT_MATH.md` —
   this is the novel math; external eyes on it strengthens confidence.
4. **Gas / CU analysis** of the matcher hot path — does each fill fit
   in the 200k CU budget? Spot checks on apply_fill, liquidate, settle.
5. **Replay simulation** against historical Hyperliquid or Drift fill
   data — would Flash Book's risk engine have prevented known
   incidents (Hyperliquid Oct 10 2025 ADL, JELLY, POPCAT)?

## Engagement logistics

- **Code freeze**: tagged release `vX.Y.Z` with frozen `Cargo.lock` +
  `bun.lock`. No new commits during audit window.
- **Communication**: GitHub Issues for low-severity, encrypted email
  for critical findings.
- **Disclosure**: report embargo until fixes deployed + 14-day grace
  period for users to migrate.

## Bounty program

Not yet established. Recommendation: after first external audit,
launch on Sherlock or Immunefi with:
- $50k pool for Critical
- $20k for High
- $5k for Medium
- Out-of-scope: gas optimization, UI bugs, theoretical attacks on
  Solana itself.

## Contact

Maintainer: see `Cargo.toml`. Audit RFP / questions: open a GitHub
Issue tagged `audit-question`.
