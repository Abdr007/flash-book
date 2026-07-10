# Security audit — adversarial re-audit 2026-07-10, WAVE 2

Companion to `docs/SECURITY_AUDIT_2026-07-10.md` (wave 1: settlement, margin/liq,
collateral/vault, access-control, oracle/arith/hypertree). Wave 2 covers the four
surfaces wave 1 did not deeply exercise, same method: independent reviewers, disjoint
surfaces, concrete-exploit-or-"no-finding", each re-verified against source.

## Verdict

**No CRITICAL. Two HIGH — both bad-debt-adjacent, fail-safe-fixable, devnet-gated;
neither is direct theft.** DoS/compute-exhaustion returned **no exploitable
finding** (every critical-path loop is bounded). ER/cross-domain and v3-vault
share/NAV math are sound.

| Surface | Result |
|---|---|
| ER / cross-domain / delegation | Attestation authenticity, delegation seeds, corrupt-book fail-closed, force-undelegate all clean. **1 MED (attestation-lag trust residual) + LOW/INFO.** |
| V3 strategist-vaults + FLP-v3 | Share/NAV math a faithful copy of the audited singleton (no donation/inflation/first-depositor; floors pool-favorable). **1 HIGH (`vault_place_order_v3` intake-gate) + 1 MED (`record_flp_fill_v3` trust) + LOW.** |
| Advanced order injection | Trigger/TWAP fire predicates, slippage-at-fire, reduce-only clamp, OCO, idempotency all clean. **1 HIGH (intake-gate, item 4.8) + 2 MED (OI cap, oracle band) + LOW.** |
| Economic / MEV / DoS / compute | Compute bounds, liquidation-reward cap, insurance isolation, FLP math all clean. **1 HIGH (liquidation-cancel dodge) + 1 MED (funding snapshot) + LOW/INFO.** |

## HIGH findings

### H-A (HIGH) — maker-open paths skip the intake initial-margin gate (bad debt → insurance)
`assert_intake_initial_margin` (`lib.rs:20278`) is called by `place_limit_v2_core`
(`:13704`) and `place_taker_order_v2` (`:1685`) **because settlement cannot
re-check margin** — the handler comment at `:20265` states a position opened with
no collateral is a "FREE OPTION … bad debt is socialised to insurance." **Six
other maker-open paths omit it:**
- `execute_trigger_order_v3` (entry branch), `execute_twap_slice_v3`,
  `place_iceberg_order_v3`, `replenish_iceberg_v3`, `place_bracket_order_v3`
  (parent leg) — **roadmap item 4.8**;
- `vault_place_order_v3` (`:11435`).

**Exploit:** deposit dust, inject a large resting order via any of the six; a
second wallet crosses it (its own taker leg is IM-gated, the maker's is not); at
`apply_fill` the attacker opens a position vastly exceeding its collateral — a free
option whose tail loss socializes to insurance/FLP. Confirmed by two independent
reviewers + the code's own comment; the reduce-only branches and the margin-gated
`basket` path are correctly exempt/covered.
**Fix (devnet cycle):** a shared `assert_injection_intake(...)` (IM +
`assert_open_position_budget` + OI cap + oracle band) invoked by all six opening
paths, against the bound `(trader, sub_index)` state/position, exempting
reduce-only. Fail-safe (rejects under-margined opens). Must land with a devnet
deploy + acceptance run — it is a 6-handler pricing/gating change with a reduce-only
exemption, not a blind push.

### H-B (HIGH) — liquidatee can cancel the injected liquidation-close order
`liquidate_position_v2` injects a **resting** GTC synthetic-close order
(`order_type: 3`, `expires_at_slot: 0`) **owned by the liquidatee** (`lib.rs:9187,
9216`) — it waits for a taker to cross. `cancel_v2_core` (`:13777`) removes any
owner-matching node after only an ownership check — **no `order_type == 3`
exclusion and no health gate**; `cancel_all_v2` (`:2461/2470`) and the reaper are
the same. The liquidatee spams `cancel_all_v2` each slot, sweeping the close before
a taker crosses; for a bankrupt position the keeper reward is 0, so re-injection
isn't incentivized → liquidation is dodged, a free option retained.
**Mitigation:** ADL (`auto_deleverage`, bankruptcy-gated) remains the backstop for
true bankruptcy, so bad debt is not permanent — this is a delay/free-option leak,
not an unbounded drain.
**Fix (devnet cycle):** refuse owner-cancel/modify of `order_type == 3` nodes (only
a fill or a keeper/authority retires them) **and** add a keeper/authority
retirement path so a stale liquidation order cannot strand; or reduce the position
atomically at injection. Devnet acceptance required.

## MED findings

- **MED (ER attestation-lag)** — no on-chain guarantee that a margin-reserving ER
  order is attested (`er_active=1`) before it can fill; order intake checks only
  that the attestation *account exists* (`er_margin_ready`), so a trader could act
  in the accept→L1-attest lag window. Inherent to the async ER trust boundary;
  mitigated by the honest-sequencer model (attest-before-ack). **Harden:** require
  attest-confirm before an ER order rests/fills + an on-chain max-staleness bound on
  xdomain withdraw.
- **MED (`record_flp_fill_v3` trust)** — authority-gated and cannot be forged by an
  outside user, but a compromised sequencer/authority can inflate FLP `realized_pnl`
  (→ NAV) with no on-chain link to a real vault-entering counterparty loss. Sequencer
  trust concentration; **harden:** commit fills against an attested book-state root.
- **MED (funding snapshot)** — `crank_funding` samples the instantaneous
  `mark−oracle` premium over `dt` with no TWAP and no min-interval; a momentary
  band-edge premium stamps a full-period tick (bounded by `rate_max`/band).
  **Harden:** premium TWAP accumulator + min crank interval.

## LOW / INFO (see the fix queue)

OI-cap bypass and oracle-band bypass on the injection paths (fold into H-A's
`assert_injection_intake`); v3 vault/FLP withdraw `er_active` check (wave-1 L-2);
`fee_tiers` not commitment-bound (wave-1 L-3); dormant-sibling liquidation dodge
(wave-1 L-1); `reset_er_margin_attestation` authority-power / attestor
non-rotatable; unarmed-FLP fill price unbounded when staleness disabled; canonical
PDA rent-leak (no close ix); `apply_fill` haircut PDAs not `init_if_needed`;
`flp_refresh_quotes` full-book walk (physically bounded, rate-limited);
`verify_collateral_solvency` O(n²) dedup (view ix, caller-paid).

## DoS / compute-exhaustion — no exploitable finding

Every loop on a critical path (liquidation, withdraw, settle, undelegate) is bounded
by a named constant or an exact-count `require!` tied to a capped value. Load-bearing
defenses verified present + correct: the taker-matcher `scan_limit = walk_limit*4`
(`lib.rs:1832`, caps *nodes scanned* not just matches — defuses the phantom-order
arena-scan attack) and `assert_open_position_budget` (`open_positions < 16`,
`:20357`, bounding every exact-count `remaining_accounts` walk), with
`MAX_STRESS_SCENARIOS = 133` as the assess_margin backstop. Reap/cancel/committee/
basket/privacy caps all named constants.

## Fix queue (next devnet-verified cycle — no defer lane)

1. **H-A** — `assert_injection_intake` on all six opening-maker paths (closes 4.8 +
   the OI-cap + oracle-band MEDs together).
2. **H-B** — protect `order_type==3` from owner-cancel + keeper/authority retirement.
3. **M-2** (wave 1) — `effective_health_mark` in withdraw/sweep.
4. MED — ER attest-before-fill + staleness bound; funding TWAP; FLP-fill state-root.
5. LOW/INFO — per the lists above.

Every item closes with a devnet deploy + a live acceptance run per the standing
deploy+live-re-verify discipline. None is a CRITICAL or a direct-theft path; the
current proof + fail-closed posture holds while the queue is worked.
