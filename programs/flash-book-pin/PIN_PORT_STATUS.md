# Pinocchio Port — Status

A faithful, zero-alloc `no_std` (on the SBF target) Pinocchio port of the Anchor
`flash-book` program, living side-by-side in the same repo. Independent program
(its `program_id` comes from the runtime — no hardcoded `declare_id!`), so it
deploys to its own address and never collides with the Anchor program.

**Progress: 89 / 134 instructions** (Ix tags `0..=88`). The Anchor program is
unchanged.

## Port conventions

- `#![cfg_attr(target_os = "solana", no_std)]`; 1-byte `Ix` tag dispatch.
- Accounts are `#[repr(C)]` pods, pointer-cast from the input buffer (no Borsh).
  Every account has a compile-time `assert!(size_of::<T>() == N)`.
- **128-bit fields are stored as `[u8; 16]` LE byte arrays** (read via
  `from_le_bytes`), never native `u128`/`i128` — Solana account data is only
  8-aligned, so a 16-aligned field is incompatible with the disc+8 offset.
- New fields are carved from each account's `_reserved`, keeping total size
  constant (no migration). Layouts are grouped-by-alignment, padding-free.
- Secure-by-default guard layer: `assert_signer` / `assert_owned_by` /
  `assert_pda` / `assert_disc` / `assert_market` / `assert_uninitialized` /
  `assert_position`, applied before any pointer-cast.
- Pure math (risk, fill, fees, tiers, envelope, haircut, side-accrual, leverage,
  solvency, liquidation) is **host-unit-tested** (406 tests); handlers are
  SBF-only and verified by `cargo build-sbf` + anchor parity.
- CI gates every merge on 4 checks: Rust, `cargo build-sbf`, Kani, Lean. The
  Kani check runs the Anchor proofs **and** this crate's **12 harnesses** (5
  haircut, 2 open-interest conservation, 5 liquidation health-price), so the
  port's formal invariants are CI-enforced on every PR.

## Conservation audit + liquidation FV (2026-06-28)

A focused audit of the already-ported settlement/fund surface for value-
conservation correctness. Three real bugs found and fixed, plus hardening and
formal/property coverage:

- **`apply_fill` open-interest** — added the fill size to ONLY the taker side,
  breaking `long_oi == short_oi` (the invariant `verify_market_invariants` auto-
  pauses on). Fixed via host-tested `fill_math::oi_after_leg` (remove old + add
  new, per leg), Kani-proven (`proof_fill_two_leg_oi_balanced`).
- **`apply_flp_fill` open-interest** — same class; the FLP pool is the maker
  with no on-chain position. Fixed via `oi_after_flp_fill` (taker leg + pool
  mirror), Kani-proven (`proof_flp_fill_oi_balanced`).
- **`settle_funding` RISK-1** — funding (entry-priced, lazy) is NOT zero-sum, so
  it must move the haircut solvency residual `V − C_tot − I`; the path omitted
  this entirely. Added the residual delta (threading the haircut PDA) + an i64
  clamp (a received credit past `u64::MAX` had wrapped) + isolated/cross routing.
- **`update_oracle`** — added an OPTIONAL envelope rate-limit gate so a buggy/
  compromised sequencer cannot jump the mark far enough in one update to mass-
  liquidate or drain via funding.
- **Liquidation pure-math foundation** — `assess_margin` (health) +
  `worse_of_health_price` / `health_price_with_staleness` (conservative dual-
  source price, P-LIQ-1/2, 5 Kani proofs) + `compute_shortfall` (bankruptcy
  resolution, 50k-case invariant fuzz). Only the fund-moving liquidation
  instruction wiring remains.

Audited clean (no bug): deposit / withdraw / transfer collateral (C_tot-neutral),
FLP-v3 deposit/withdraw (capital-based shares, round-trip never creates value —
200k-case test), insurance withdraw (floored at pause threshold), haircut
convert / release (exact value conservation), the `place_taker_order` matcher
(book-only; settlement is `apply_fill`), and the fee path.

## Ported instructions (89)

### Core lifecycle & matching-settlement (hardened)
`ApplyFill` (0), `SettleFunding` (1), `PlaceLimitOrder` (2), `CancelOrder` (3),
`PlaceTakerOrder` (4), `ModifyOrder` (5), `CancelAll` (6), `ApplyFlpFill` (7).

### Trader / collateral
`OpenTraderState` (8), `DepositCollateral` (10), `WithdrawCollateral` (12),
`OpenTraderSubAccount` (16), `TransferCollateral` (17),
`CloseTraderSubAccount` (18), `InitTraderAta` (71), `CloseTraderAta` (72).

### Market / insurance governance
`InitializeInsuranceFund` (9), `InitializeMarket` (11), `SetMarketSequencer` (13),
`SetMarketStatus` (14), `UpdateOracle` (15), `SetMarketParams` (20),
`TransferMarketAuthority` (21), `TransferInsuranceAuthority` (22),
`SetInsuranceFeeContribution` (23), `SetMarketMaintenanceMargin` (25),
`SetMarketRiskParams` (36), `SetMarketMaxLeverage` (45),
`SetInsurancePauseThreshold` (54), `BurnMarketAuthority` (55).

### Trader config
`SetTraderFeeTier` (19), `SetTraderDelegate` (37), `SetTraderReferrer` (38),
`SetTraderBuilder` (39), `SetPositionLeverage` (46).

### FLP capital
`InitializeFlpExposure` (26), `InitLpPosition` (27), `DepositFlpCapital` (28),
`WithdrawFlpCapital` (29), `InitFlpPerMarket` (53).

### Read-only risk surface (all on the proven `assess_margin`)
`VerifySolvency` (24), `VerifyProtocolSolvency` (30), `VerifyMarketInvariants`
(31), `VerifyCollateralSolvency` (32), `VerifyStressSolvency` (40),
`VerifyPortfolioSolvency` (41), `VerifyStressLattice` (44),
`VerifyPortfolioStress` (47), `VerifyLeverageCap` (48).

### Validated config tables + re-validation
`InitMarketLeverageTiers` (34), `UpdateMarketLeverageTiers` (35),
`InitFeeTiers` (42), `UpdateFeeTiers` (43), `SetEnvelopeConfig` (56),
`InitMarketOracleConfig` (58). Re-validation probes: `VerifyEnvelopeConfig` (57),
`VerifyOracleConfig`* (74), `VerifyLeverageTiers`* (75), `VerifyFeeTiers`* (76).

### ER liveness & cross-domain margin
`ErHeartbeat` (33), `InitErMarginAttestation` (66), `AttestErReservedMargin` (67).

### Conditional / TWAP orders (placement)
`PlaceTriggerOrder` (49), `CancelTriggerOrder` (50), `PlaceTwapOrder` (51),
`CancelTwapOrder` (52).

### Risk subsystems (ADL / envelope / haircut)
`InitializeSideAccrual` (59), `VerifySideAccrualInvariants`* (73),
`InitializeHaircutState` (61), `VerifyHaircutInvariants` (62),
`InitPositionHaircutState` (63), `MaturePosition` (68), `SeedResidual` (69),
`VerifyPositionHaircut`* (78), `GateEnvelopePriceMove` (70).

### Vaults / session keys
`CreateVault` (60), `CreateSessionToken` (64), `RevokeSessionToken` (65),
`VerifySessionActive`* (77).

> `*` = port-addition (a read-only enforcing probe with no standalone Anchor
> counterpart; re-uses the exact host-tested write-time validator).

## Custom error codes (revert reasons)

| Code | Instruction | Meaning |
|---|---|---|
| 100 | VerifySolvency | position below maintenance margin |
| 101 | VerifyProtocolSolvency | vault < insurance + FLP capital |
| 102 | VerifyCollateralSolvency | partial collateral proves insolvency |
| 105 | VerifyMarketInvariants | open-interest imbalance (auto-paused) |
| 107 | VerifyMarketInvariants | ER-stall liveness breach (auto-paused) |
| 110 | VerifyStressSolvency | breach under the supplied shock |
| 111 | VerifyStressLattice | breach under the worst lattice shock |
| 112 | VerifyPortfolioStress | portfolio breach under a shock |
| 113 | VerifyLeverageCap | notional exceeds cap × collateral |
| 120 | VerifyEnvelopeConfig | stored params fail the envelope proof |
| 121 | VerifyHaircutInvariants | haircut state inconsistency |
| 122 | MaturePosition | nothing to mature this slot |
| 123 | GateEnvelopePriceMove | price move exceeds the per-slot band |
| 124 | VerifySideAccrualInvariants | `a > ADL_ONE` or invalid mode |
| 125 | VerifyOracleConfig | stale/confidence/source bounds violated |
| 126 | VerifyLeverageTiers | stored ladder fails validation |
| 127 | VerifyFeeTiers | stored fee table fails validation |
| 128 | VerifySessionActive | session revoked or expired |
| 129 | VerifyPositionHaircut | warmup-accumulator invariant violated |

## Remaining (55) — deferred to focused, attended sessions

These move funds, mutate the order book, run matching/liquidation, or do
cross-program (ER/Pyth/ATA-of-book) CPIs, and are **not** safe to do unattended:

- **Order book / matching:** `place_limit_order_v` (book insert),
  `place_taker_order_v`, `place_iceberg_order`, `replenish_iceberg`,
  `cancel_iceberg`, `place_basket_order(_n)`, `place_bracket_order`,
  `execute_trigger_order`, `execute_twap_slice`, `reap_expired_orders`,
  `init_market_book`, `expand_market_book`, `stamp_book_liveness_baseline`,
  `view_book_depth`, `view_quote_ladder`.
- **Liquidation / ADL execution:** `liquidate_position`, `liquidate_portfolio`,
  `auto_deleverage`, `place_jit_liquidation_offer`,
  `cancel_jit_liquidation_offer`, `convert_position`, `settle_mark`.
- **Fund flows:** `flp_deposit`, `flp_withdraw`, `record_flp_fill`,
  `partial_withdraw_collateral(_xdomain)`, `withdraw_collateral_xdomain`,
  `sweep_collateral`, `transfer_main_to_sub`, `transfer_sub_to_main`,
  `withdraw_insurance_fund`, `flush_haircut_dust`, `release_gain_to_haircut`,
  `deposit_collateral_session`, `set_position_cross`, `set_position_isolated`.
- **Vaults (fund/book):** `vault_deposit`, `vault_withdraw`, `vault_place_order`,
  `vault_cancel_order`, `vault_open_trader_state`, `settle_vault_perf_fee`.
- **ER / MagicBlock CPIs:** `delegate_market(_book)`, `undelegate_market(_book)`,
  `force_undelegate_market_book`, `commit_market_book`,
  `commit_and_undelegate_*`, `init/delegate/undelegate/commit_fill_commitment`,
  `process_undelegation`, `init_book_permission`, `close_book_permission`,
  `set_book_privacy`.
- **External oracle (misaligned with the simplified sequencer-set mark):**
  `update_oracle_from_pyth`, `update_oracle_from_lazer`, `update_oracle_quorum`.
- **Other:** `update_trailing_stop` (needs trigger fields that don't fit the
  size-locked account), `update_market_params` (Anchor bulk struct — the port
  uses per-field setters), `migrate_*`, `mature_position` consumers, the `view_*`
  reporters (no events/return-data in the port).

**Recommendation:** the next focused session should take **one** pillar — the
order-book matching path (`place_limit_order` against the hypertree, unblocks the
most downstream instructions) or the liquidation engine (loss-conservation
correctness) — with the user attended and the Kani/Lean FV harness in the loop.
