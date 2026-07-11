# Security audit — adversarial re-audit 2026-07-10

A five-dimension adversarial re-audit of the deployed Anchor program
(`5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq`), run as five independent
reviewers over disjoint surfaces, each instructed to find concrete exploits (not
vague risks) or explicitly report "no finding" — never invent. Every finding was
re-verified against the source before disposition.

## Verdict

**No CRITICAL or HIGH exploitable finding on any surface.** The
access-control/account-model, oracle-ingestion, arithmetic, and hypertree-book
surfaces returned **zero exploitable findings**. Settlement/fill-authenticity
returned zero value-mint/fabrication/replay/reorder defects. Two MEDIUM
defense-in-depth gaps and a set of LOW/INFO hardening notes were found on the
collateral-release paths — both MEDs harden withdrawal in the **fail-safe
direction** (they can only ever *reject* a release, never permit an unsafe one),
so protocol fund-safety cannot regress from either.

| Surface | Result |
|---|---|
| Access control / account model | **No exploitable finding** — every privileged path gated; every account seed/owner/disc-bound; sub-account, session, and settlement-authority isolation verified individually. |
| Oracle ingestion | **No exploitable finding** — staleness + confidence + feed-id/source binding + ed25519/Lazer signer binding + monotonic replay guards; mark-EMA clamped into the trustless oracle band. |
| Arithmetic | **No exploitable finding** — all 39 narrowing money casts are bounds-checked; funding index Kani-proven gated-and-safe; `>>64` truncation is a transfer direction, never a mint. |
| Hypertree book | **No exploitable finding** — `validate_node_links` (cycle-free DFS, bounds, free-list disjointness) at the ingest choke point; corrupt book fails closed, never lands on L1. |
| Settlement / fill authenticity | **No fabrication/replay/reorder/mint** — keccak commitment ring binds every fill field; consume-and-clear FIFO + monotonic nonce; sequencer fail-closed on zero pubkey. 1 LOW + 1 INFO (below). |
| Collateral / vault / conservation | Reserve-margin gate sound & fail-closed on all six release paths; conservation holds; insurance waterfall bounded by its own balance. **1 MED (fixed) + 1 LOW.** |
| Margin / liquidation / ADL | Worse-of health pricing, margin-walk completeness, ADL bankruptcy gate & value-conservation all clean. **1 MED + 1 LOW/MED + 1 INFO.** |

## Findings & dispositions

### M-1 (MED) — `set_position_isolated` did not honor `er_reserved`  ✅ FIXED
`lib.rs` `set_position_isolated`. An ER-active trader (attested reserved margin
backing resting ER orders in market B) could relocate cross collateral into a
market-A isolated bucket, draining the cross pool below the market-B reservation
and walling that collateral off from B's settlement — dumping bad debt onto
insurance. Total collateral was unchanged, so no direct theft, but it defeats the
withdraw-anytime reserve-margin bridge via isolation instead of withdrawal.
**Fix (this PR):** gate on `er_active == 0` (`FlashBookError::ErMarginReserved`),
mirroring the identical `withdraw_collateral` / `partial_withdraw_collateral` /
`sweep_collateral` gates — an ER-active trader must resolve the ER (undelegate /
cancel resting orders) before isolating. Fail-closed; a strict no-op for every
trader that never touched the ER. Behavior-preserving for the common path (363
host tests green).

### M-2 (MED) — withdraw/sweep value positions off the RAW mark (no worse-of / staleness)  ⏳ QUEUED for the next devnet-verified cycle
`partial_withdraw_core` (`lib.rs:13439`) and `sweep_collateral` (`lib.rs:3745`)
build their `RiskMarketSnap` with `mark_price: Ticks(market.mark_price_ticks)` —
the raw EMA mark — with **no oracle worse-of and no staleness gate**, unlike every
liquidation/ADL path (which routes through `effective_health_mark`). On a stalled
or illiquid market whose frozen mark sits above the true price, a trader holding a
losing long can over-withdraw against the inflated mark; the shortfall is
socialized to insurance when the position is later liquidated at the true price.
No manipulation is required — passive mark/oracle divergence suffices.
**Exact fix:** value both handlers' per-leg snap off
`effective_health_mark(&market, now_unix, current_slot, position.side == 0)`
(worse-of + staleness), exactly as `liquidate_portfolio_v2` does at `lib.rs:9889`.
Verified safe direction: for no-oracle markets with a fresh mark, `effective_health_mark`
returns the mark unchanged (no behavior change); for oracle-configured markets it
returns the worse-of (more conservative); for a stale mark with no fresh oracle it
reverts (fail-safe) rather than over-withdrawing.
**Why queued, not shipped here:** this changes *pricing* on the two most critical
value-release paths. It is fail-safe for funds, but a regression could revert
legitimate withdrawals (an availability change on stale-oracle markets). Per the
standing discipline (*deploy + live-re-verify every program change*), it must land
with a devnet deploy + a live stale-market acceptance run, not a blind push. It is
the **#1 priority** for the next devnet cycle.

### L-1 (LOW/MED) — dormant/stale-oracle sibling makes a cross trader un-portfolio-liquidatable
`liquidate_portfolio_v2` (`lib.rs:9889`). Because the portfolio walk requires
every leg and calls `effective_health_mark` on each, a single leg on a
misconfigured dormant market (staleness disabled + no recent fills) reverts the
whole instruction — so genuinely-underwater positions on other markets dodge
portfolio liquidation. The fail-safe "don't liquidate on untrusted prices" stance
turned into a griefing lever. **Fix:** for a stale-priced sibling, fall back to a
conservative worst-case valuation of that leg rather than aborting the whole
liquidation; or require staleness config on every listed market. Devnet cycle.

### L-2 (LOW) — v3 vault / FLP-v3 withdraw lack an `er_active` check
`withdraw_vault_v3` (`lib.rs:11340`), FLP-v3 withdraw (`lib.rs:11930`). Both
release from the shared vault against a PDA-owned balance without consulting
`er_active`/`er_reserved`. Only reachable if a strategy PDA rests ER orders
(unusual). **Fix:** add the same `er_active == 0` assertion for symmetry. Devnet cycle.

### L-3 (LOW) — `fee_tiers` is keeper-selectable, not commitment-bound  ✅ ACCEPTED RESIDUAL (documented; fix specified + trigger-gated)
`ApplyFill`/`ApplyFlpFill` contexts. On an armed (permissionless) market a keeper
can omit the `Option<fee_tiers>` account, forcing the flat `market.params`
fee/rebate instead of the traders' volume-tier rates. No value is minted or stolen
by the keeper — the delta flows to insurance, not the attacker — and it only applies
if the griefer out-races the honest sequencer. Pure fairness/griefing.

**Disposition (2026-07-11): accepted residual, not fixed now.** The griefing surface
is **latent** — it exists only once a *nonzero* fee-tier table is activated: the
tier table is a **global singleton** (`FeeTiersAccount`, PDA `[b"fee_tiers"]`,
`tier_count` rows), there is **no per-market tier signal** on `MarketAccount`/
`MarketParams`, and the singleton is **not part of mandatory genesis**. Until
`init_fee_tiers` is run with `tier_count > 0`, `resolve_fee_tier` falls back to flat
`market.params` for everyone, so an omission changes nothing. Every actual fix is
disproportionate to a LOW, latent, no-theft griefing bug: (a) a `MarketAccount`
layout grow for a `fee_tiers_required` flag breaks deserialization of existing
fixed-size markets (migration); (b) binding a `fee_tiers_present` bit into the
settlement **keccak preimage** modifies the formally-reasoned fill-authenticity ring
(proven-core-adjacent); (c) making the singleton **mandatory** on all armed
`apply_fill` is a breaking settlement-interface change that reverts every market
without the singleton initialized.

**Fix to apply WHEN fee tiers are activated pre-launch** (the trigger that makes this
non-latent): initialize the `[b"fee_tiers"]` singleton as a standing genesis account
(default `tier_count = 0`), then make `fee_tiers` a **required** account on
`ApplyFill`/`ApplyFlpFill` and require the caller to pass the canonical PDA. With the
singleton always present, an honest keeper always supplies it (public account, zero
cost) and a griefer can no longer strip tiers — with no per-market flag, no migration,
and no keccak change. Tracked in the roadmap fix queue against the fee-tier-activation
milestone.

### INFO notes (not exploitable — recorded for hardening)
- **I-1** `partial_withdraw_core` withdraw floor sums the *isolated* required-margin
  against the *cross* pool (`lib.rs:13462`) — strictly over-conservative (only
  blocks legitimate withdrawals, never permits an unsafe one). Gate on
  `cross.required` for the cross-pool withdrawal.
- **I-2** ring/outbox located by (owner, disc, market) rather than a re-derived PDA
  (`find_fill_commitment`/`find_fill_outbox`) — mitigated by disc-uniqueness; a
  `create_program_address` assert would make it self-evidently correct.
- **I-3** Lazer `skip_prop` assumes 2-byte unknown properties — correct today;
  length-prefix if Lazer's extension space is adopted.
- **I-4** `update_oracle_from_lazer` parses but does not gate `px.channel` — nil
  impact (all channels share the trusted signer + price); a bound tightens intent.
- **I-5** `initialize_market` stores vestigial `base_vault`/`quote_vault`/
  `oracle_account` unchecked — custody flows only through the address-pinned
  `insurance_fund.quote_vault`; authority-gated creation, no untrusted consumer.

## Confirmed-clean invariants (adversarially verified, no finding)

Fill fabrication/redirection/reorder (armed); replay/double-settle/off-by-one;
sequencer zero-pubkey fail-closed; book↔FLP handler separation; settlement account
substitution; reduce/flip/stack position math; taker self-trade/crossing/tick;
reserve-margin gate on withdraw/partial/sweep/transfer/xdomain/session; token-CPI
substitution & signer seeds; V = C_tot + I + Residual conservation; FLP NAV
dilution / JIT lock / flat-gate; residual over-statement; ADL bankruptcy gate &
`.min(loss)` value conservation; isolated↔cross double-counting; cross-market
netting; liquidation reward / bad-debt routing; margin-walk completeness;
oracle staleness/confidence/feed-binding; ed25519/Lazer signer binding; monotonic
oracle & funding replay guards; mark-EMA band clamp; all 39 narrowing money casts;
hypertree `validate_node_links`; cancel/modify ownership; every privileged-instruction
authority gate; sub-account & session isolation; init/re-init front-running.

## Fix queue (next devnet-verified cycle)

1. **M-2** — route `partial_withdraw_core` + `sweep_collateral` valuation through
   `effective_health_mark` (worse-of + staleness). Highest priority.
2. **L-1** — conservative sibling-leg fallback in `liquidate_portfolio_v2`.
3. **L-2** — `er_active == 0` on v3 vault / FLP-v3 withdraw.
4. **L-3** — bind `fee_tiers` into the fill commitment.
5. **I-1..I-5** — over-conservative floor tidy, PDA-derivation asserts, Lazer
   robustness, channel gate.

Each closes with a devnet deploy + a live acceptance run per the standing
discipline; none blocks the current proof/hardening posture.
