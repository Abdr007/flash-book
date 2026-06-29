# Flash Book — devnet transaction audit (real on-chain data)

Audited 2026-06-28. Every fact below was pulled live from devnet RPC (`getTransaction`,
`getAccountInfo`) and decoded against `idl/flash_book.json`. No values are estimated unless
explicitly marked.

## 0. Provenance (verified)

| Item | Value |
|---|---|
| Program | `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (BPFLoaderUpgradeable, executable) |
| Program data size | 2,292,413 bytes (2.29 MB) |
| Upgrade authority | `GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD` — **same key as the demo signer** |
| Last deploy slot | 472,312,797 (~12.5 h before the demo session) |
| IDL surface | **129 instructions, 125 events, 28 accounts, 163 types, 91 error codes** |
| Demo session | slots 472,425,946 → 472,432,012 (span 6,066 slots ≈ **40 minutes**), 2026-06-27 23:13–23:51 UTC |
| Signer / market | one signer, one market `3UWaYaqCk…U66q`, one sub-account |

## 1. Headline result

- **47 / 47 transactions landed successfully.** Zero `err`, every one emits `Program … success`.
- **47 / 129 instructions exercised = 36% of program surface.**
- Total compute: 512,693 CU across 47 ix; mean **10,908 CU**; range 1,511 → 26,551.
- Total fees: 0.00024 SOL (all base 5,000-lamport sigs; **no priority fee, no compute-budget ix** on any tx).
- Events decode cleanly with **real economic values** (prices, sizes, balances, PnL) — not placeholder stubs.

## 2. The one finding that matters most

**No fill, no position, no funding, and no liquidation ever happened on-chain.**

The decoded events prove the order *plumbing* works but the *trading core* was never integrated-tested:

- `place_taker_order_v2` → `TakerOrderClearedEvent { taker_size_lots: 8, filled_lots: 0, match_count: 0, residual_resting_lots: 8 }` — the taker crossed **nothing** and rested in full. The book held only a single resting **bid** (side=0, 99000×10) and the taker was also side=0, so by construction it could never match.
- `view_book_depth_v2` → `total_orders_active: 0, bids: [], asks: []` at snapshot time.
- `view_portfolio_risk` → `open_positions: 0, equity: 9,400,000, health_ratio_bps: 4,294,967,295` (u32::MAX = "no risk").
- `settle_mark` → old = new = oracle = 100000 (no price move); `view_predicted_funding` → premium 0 bps, rate 0.
- **Census of all 125 event types across the 47 tx: not one** `FillApplied`, `FillBatch`, `FundingSettled`, `Liquidat*`, `AutoDeleveraged`, `BadDebtSocialized`, `MarginThresholdCrossed`, or `PositionConverted/Matured`.

So this is a **breadth/coverage demo** ("every instruction type can land on the deployed program"),
**not** an integrated venue demo ("a taker matched a maker, a leveraged position opened, funding
accrued, and someone got liquidated"). The hardest and most valuable parts of a perps DEX — the
matching engine under a crossing book, the margin engine, and the liquidation/ADL/bad-debt waterfall
— have **zero on-chain evidence here.**

## 3. The 82 instructions NOT demonstrated (by risk weight)

The unexercised 64% is exactly where perps DEXs live or die:

- **Risk / liquidation core:** `liquidate_position_v2`, `liquidate_portfolio_v2`, `auto_deleverage`,
  `place/cancel_jit_liquidation_offer`, `seed_residual`, haircut suite
  (`initialize_haircut_state`, `release_gain_to_haircut`, `verify_haircut_invariants`, `flush_haircut_dust`),
  `verify_collateral_solvency`, `verify_protocol_solvency`, `withdraw_insurance_fund`.
- **Funding & fills:** `settle_funding`, `apply_fill`, `apply_flp_fill`, `record_flp_fill_v3`.
- **Position lifecycle:** `set_position_cross/isolated/leverage`, `convert_position`, `mature_position`, `migrate_*`.
- **Advanced orders:** TWAP, iceberg (v2/v3), bracket, trigger (v2/v3), basket — ~18 instructions, none run.
- **Vaults v3:** the entire `*_v3` vault suite.
- **ER lifecycle:** only `er_heartbeat` + `init_fill_commitment` ran; `delegate_*`, `commit_*`,
  `undelegate_*`, `commit_and_undelegate_*`, `process_undelegation` did **not** — i.e. the rollup
  delegation round-trip was not closed on-chain.
- **Oracle:** `update_oracle_from_pyth` (the real Pyth path) was not used; `update_oracle` /
  `update_oracle_quorum` wrote values manually with no `OracleUpdatedFromPythEvent`.

## 4. Per-transaction ratings

Grade key: **A** = real, economically-meaningful state change verified via decoded event/token delta ·
**B** = real but trivial/config or zero-magnitude write · **R** = read-only view (graded on whether it
returned real data) · all 47 = landed OK.

### Setup & accounts (real rent paid, verified)
| ix | CU | evidence | grade |
|---|---|---|---|
| open_trader_state | 6,664 | −0.00229 SOL rent, SystemProgram CPI | A |
| initialize_insurance_fund | 20,357 | rent + Token CPI (vault init) | A |
| initialize_flp_exposure | 20,105 | `FlpExposureInitializedEvent`, −0.0084 SOL | A |
| init_fee_tiers | 8,868 | `FeeTiersInitializedEvent` | A |
| open_trader_sub_account | 14,542 | `SubAccountOpenedEvent` | A |
| init_trader_ata | 25,578 | ATA-program CPI, real token account created | A |
| set_trader_referrer/delegate/builder | 1,766 / 1,800 / 1,839 | distinct events, sub-2k CU | A |
| set_trader_fee_tier | 3,946 | `TraderFeeTierUpdatedEvent` | A |
| create / revoke_session_token | 7,633 / 4,022 | `SessionTokenEvent`; revoke refunds +0.00145 SOL | A |

### Market creation & admin
| ix | CU | evidence | grade |
|---|---|---|---|
| initialize_market | 23,353 | `MarketInitializedEvent`, 11 accounts | A |
| init_market_book | 9,891 | `MarketBookInitializedEvent`, −0.0695 SOL (large book alloc) | A |
| expand_market_book | 10,238 | `MarketBookExpandedEvent`, −0.0428 SOL | A |
| init_market_leverage_tiers | 13,849 | tiers event | A |
| init_market_oracle_config | 11,496 | oracle-config event | A |
| update_market_params | 11,345 | `MarketParamsUpdatedEvent` | A |
| set_market_sequencer | 10,709 | `MarketSequencerRotatedEvent` | A |
| set_market_status | 10,555 | `MarketStatusChangedEvent` | A |
| transfer_market_authority | 10,685 | `MarketAuthorityTransferredEvent` | A |

### Oracle & risk envelope
| ix | CU | evidence | grade |
|---|---|---|---|
| set_envelope_config | 16,273 | `EnvelopeConfigSetEvent` | A |
| verify_envelope_config | 6,693 | `EnvelopeVerifiedEvent` | B (assertion pass) |
| gate_envelope_price_move | 3,500 | no event; gate check only | B |
| update_oracle | 14,269 | **no event emitted**; manual price write | B |
| update_oracle_quorum | 14,587 | **no event** | B |
| settle_mark | 10,880 | `MarkPriceUpdatedEvent` but old==new==100000 (**zero-magnitude**) | B |
| verify_market_invariants | 10,408 | no event; invariant assertion | B |

### Collateral & FLP (real SPL token movement — strongest evidence)
| ix | CU | evidence | grade |
|---|---|---|---|
| deposit_collateral | 10,950 | **Token CPI, balance 5.5→10.5**, `CollateralDepositedEvent{amount:5_000_000,new_balance:9_450_000}` | A |
| withdraw_collateral | 11,025 | **Token CPI, 11.499→10.999**, `CollateralWithdrawnEvent` | A |
| transfer_main_to_sub / sub_to_main | 5,594 / 5,600 | `SubAccountTransferEvent` ×2 | A |
| deposit_flp_capital | 26,551 | **Token 10.5→11.5**, `FlpCapitalUpdatedEvent` (highest-CU tx) | A |
| withdraw_flp_capital | 22,267 | **Token 11.5→11.499**, `FlpCapitalUpdatedEvent` | A |

### Order lifecycle (CLOB) — plumbing real, matching unexercised
| ix | CU | evidence | grade |
|---|---|---|---|
| place_limit_order_v2 | 13,002 | `OrderPlacedV2Event{side:0, price:99000, size:10, seq:1, total_after:1}` — real book insert | A |
| modify_order_v2 | 13,232 | `OrderModifiedV2Event{new_price:95500, new_size:17, seq 6→7}` | A |
| cancel_order_v2 | 7,242 | `OrderCancelledV2Event` | A |
| cancel_all_v2 | 8,702 | `BulkOrderCancelledV2Event` | A |
| reap_expired_orders | 7,223 | `ExpiredOrdersReapedEvent` (0 reaped) | B |
| **place_taker_order_v2** | 13,582 | `TakerOrderClearedEvent{filled:0, match_count:0}` — **matched nothing** | **C** |

### Read-only views (correctly mutate nothing; returned real data)
| ix | CU | returned | grade |
|---|---|---|---|
| view_portfolio_risk | 1,511 | equity 9.4, 0 positions, health=MAX | R-A |
| view_trader_effective_tier | 5,749 | real tier struct | R-A |
| view_book_depth_v2 | 6,784 | empty book (0 orders) | R-B |
| view_predicted_funding | 8,771 | premium 0, rate 0 (no skew) | R-B |
| view_quote_ladder | 10,006 | `QuoteLadderSnapshotEvent` (empty ladder) | R-B |

### ER (base-side, incomplete round-trip)
| ix | CU | evidence | grade |
|---|---|---|---|
| er_heartbeat | 10,711 | `ErHeartbeatEvent` | B |
| init_fill_commitment | 18,340 | `FillCommitmentInitializedEvent`, −0.0156 SOL rent | A (but delegate/commit/undelegate never run) |

## 5. Compute-unit read

The CU numbers are genuinely **lean** for an Anchor program of this size — place/cancel/modify at
7–13k and views at 1.5–10k are excellent. **But** the caveat is decisive: these are
**single-order, near-empty-book** operations. The cost driver in any CLOB is *tree traversal during a
crossing match*, and `place_taker_order_v2` ran with `match_count:0`, so **the demo contains no data
on matching-engine CU under load.** A taker sweeping N price levels is the number that matters and it
was never produced. Also note: no `ComputeBudget` instruction on any tx — fine at these levels, but a
real crossing taker will need one.

## 6. Comparison to current on-chain orderbook perps (2026)

> **Critical benchmark caveat (verified across primary sources):** *real per-instruction CU integers are
> essentially unpublished across the entire field.* Drift, Phoenix, Manifest, Zeta/Bullet — none publish
> a source-backed CU for bare place/cancel/fill. Drift docs only *recommend* a 400k–800k CU **limit** for
> place-and-make (a budget ceiling, not a measurement). So Flash Book's decoded CU below cannot be
> ranked against competitor CU as fact — the honest move is that Flash Book should publish its own
> `simulateTransaction`-measured matching CU (which this demo never produced; see §5).
>
> Solana hard limits (solana.com/docs): 1.4M CU/tx cap; 200k CU/ix default; ComputeBudget has 4 ix and
> charges priority fee on the *requested* limit, not actual usage.

| Dimension | Flash Book (this demo) | Phoenix | Manifest | Drift v2 | dYdX v4 | Hyperliquid |
|---|---|---|---|---|---|---|
| Matching model | **fully on-chain atomic CLOB** (hypertree) | **on-chain atomic, crankless** | **on-chain atomic, crankless** (HyperTree/RB-trees) | **off-chain keeper crank** → on-chain `fill` ix (+ JIT auction + vAMM) | **off-chain** in-memory book, block-proposer matches | **on-chain in consensus** (own L1) |
| Perps? | yes (this program) | **spot only** (Perps = separate new codebase, private beta) | **spot only** | yes | yes | yes (leader) |
| Place-order CU | **13,002** (empty book, exact) | not first-party published | "~45% < Phoenix" (own claim, no integer) | no per-ix figure; 400–800k *limit* rec | n/a (Cosmos) | n/a (own L1) |
| Match CU under load | **unmeasured** (match_count 0) | unpublished | unpublished | unpublished | n/a | n/a |
| Oracle | manual `update_oracle` here; Pyth path (`update_oracle_from_pyth`+quorum) exists but **unused on-chain** | n/a (spot) | n/a (spot) | Pyth+Switchboard pull, 2% conf / 10–120 slot staleness, divergence guards | in-protocol Slinky (ABCI++ vote-extension median) | validator source-weighted median (~3s) |
| Risk engine | envelope + insurance + haircut + ADL **in code, none exercised** | n/a (spot) | n/a (spot) | cross+isolated, **progressive partial liq**, IF→vAMM→ADL waterfall — live | cross+isolated, partial liq @ fillable price, IF→deleverage | cross+isolated, partial liq, HLP backstop, refined ADL (post-JELLY) |
| Instruction surface | **129** | 28 | small (no-Anchor) | **~258** | n/a | n/a |
| Formal verification | **49 Kani + Lean (per project notes)** | none | **Certora, re-run daily** | **none** | none (deepest *conventional* audit: Informal Systems, 0 critical) | none public |
| Maturity | **devnet, unaudited, 40-min coverage demo** | mainnet, audited (OtterSec) | mainnet, audited+FV | mainnet, audited (ToB/Neodyme), large TVL | mainnet app-chain | mainnet perps leader |

**Where Flash Book occupies genuinely unoccupied space (now cited):** per the research pass, **no
deployed competitor pairs a fully on-chain / atomic CLOB *perps* core with first-party formal
verification.** Manifest has daily Certora FV but is **spot-only**. Phoenix is **spot-only** (its perps
product is a separate, unreleased codebase). Drift has the battle-tested risk engine but matches
**off-chain** via keepers and has **no FV**. dYdX v4 and Zeta/Bullet also push matching off-chain
(proposer / single sequencer). Hyperliquid leads perps but runs its **own L1** with **no public FV** —
and isn't a fair peer to a single Solana program (it owns consensus, VM, sequencing; no CU budget, no
write-lock contention). So the map has an open corner exactly where Flash Book aims: **on-chain atomic
CLOB perps + Kani/Lean-proven core + ER sub-50ms execution.**

**Where this demo is weakest vs. all of them:** every mainnet competitor has *real fills, real open
interest, real liquidations* happening continuously. This demo has **none**, on devnet, unaudited,
single-session. The honest framing is "widest feature surface + strongest FV posture *on paper*" vs.
"proven under adversarial live flow." Flash Book leads the former and has shown **nothing** of the
latter on-chain yet. Also note (research flags): table-stakes for 2026 = cross+isolated margin,
progressive partial liquidation, insurance fund + explicit loss waterfall/ADL, premium-index funding
with clamp/cap, multi-oracle confidence/staleness gating, fee tiers, external audit — Flash Book has
all of these **in code**, but the demo exercised the gating/fund-init only, never the liquidation/
funding/ADL paths. The JELLY incident (Hyperliquid, Mar 2025: ADL trigger computed on pooled vault
balance instead of the liquidator vault's own losses) is the canonical reason those paths must be
demonstrated, not just written.

## 7. Overall rating

| Axis | Score | Basis |
|---|---|---|
| Did the instructions execute correctly? | **9.5 / 10** | 47/47 landed, events decode to real values, real token & rent movement, lean CU |
| Breadth of surface demonstrated | **6 / 10** | 47/129 = 36%; the demonstrated set is the easy 36% |
| Trading-core proof (match/position/funding/liq) | **2 / 10** | zero fills, zero positions, zero funding, zero liquidation on-chain |
| Production credibility | **3 / 10** | devnet, unaudited, mutable upgrade authority = signer, 40-min scripted run |
| Engineering signal (CU, event design, FV posture) | **9 / 10** | clean events, tiny CU, 49 Kani + Lean per project notes |

**Bottom line:** This is a **clean, honest "the deployed program accepts every call" milestone** — and
on that narrow claim it scores ~9.5. As evidence that *Flash Book works as a perpetuals exchange*, it
is **early**: the matching engine never crossed a trade, no leveraged position ever existed, and the
entire risk/liquidation/funding waterfall — the part that actually protects an exchange — has no
on-chain footprint yet.

## 8. To make the next devnet pass credible (highest leverage first)

1. **Cross a real trade:** place a resting **ask**, then a buy taker that lifts it → emit
   `FillApplied`/`FillBatch`, open a position. This is the single most important missing transaction.
2. **Open → fund → liquidate:** open a leveraged position, move the oracle via
   `update_oracle_from_pyth`, `settle_funding`, then drive `liquidate_position_v2` + insurance/haircut.
3. **Close the ER round-trip:** `delegate_*` → fills in rollup → `commit_and_undelegate_*` →
   `process_undelegation`.
4. **Measure matching CU under a deep book** (sweep N levels) and publish that number — that's the
   figure competitors will be judged against.
5. **Use the real oracle path** (`update_oracle_from_pyth` + quorum) so `OracleUpdatedFromPythEvent` appears.
6. Note for production: upgrade authority == demo signer (expected on devnet; must become a
   multisig/timelock for any real deployment).
