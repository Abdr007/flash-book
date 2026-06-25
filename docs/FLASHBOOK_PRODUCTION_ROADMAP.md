# Flash Book — Production Roadmap

> From hardened devnet (security pass + FV suite, PR #34) to **live mainnet at
> real size**. Separates **keyboard time** (compressible) from **calendar time**
> (gated by external parties + deliberate observation windows — *not*
> compressible by working harder). Honest estimates, ranges not points.

## Phase 0 — Finish the architecture (engineering) · ~3–4 weeks
Serial — one codebase, one engineer.

| Task | Effort | Issue |
|---|---|---|
| Settlement authenticity: `fill_commitment` ring + 4 Kani proofs + producer/consumer wiring + ER delegation + FLP-quote verification | 5–7 d | #35 |
| Book-stuffing DoS: per-trader resting-order cap / resting price-band / permissionless expiry-reaper | 1–2 d | #36 |
| FV `[REQUIRE]`→`[PROVEN]`: margin-completeness, global-solvency preservation, liquidation gate, settlement-nonce monotonicity, hypertree ordering | 5–8 d | — |
| M1–M13 / L1–L5 triage + fix the real ones | 2–4 d | #37 |
| IDL regen, integration tests, merge PR #34 | 1–2 d | #38✓ |

**Output:** code-complete, machine-proven, audit-ready build. The only phase fully under our control.

## Phase 1 — External audit · 6–13 weeks calendar
| Step | Calendar | Owner |
|---|---|---|
| Audit prep (freeze, scope package, threat-model handoff) | 3–5 d | us |
| **Firm queue** (top Solana firm) | **2–6 wk** | external ← long pole |
| Audit execution | 2–4 wk | external |
| Remediation | 1–2 wk | us |
| Delta re-review + published report | 1 wk | external |

**Lever:** book the audit slot the day Phase 0 *starts*. Cuts 3–4 wk off the critical path.

## Phase 2 — Pre-mainnet hardening · 4–8 weeks (partly parallel with Phase 1)
| Track | Calendar |
|---|---|
| Devnet load/chaos harness (deep sweeps, ER churn, sequencer failover, oracle-staleness injection) | 1–2 wk build, then continuous |
| **Public bug bounty** (Immunefi-style, capped real funds, on the audited build) | **4–8 wk live** (calendar-bound) |
| Economic dry-run (seed insurance, real LP capital on devnet, funding/fee/haircut under adversarial flow) | 2–4 wk |

## Phase 3 — Guarded mainnet launch · 2–4 weeks
| Step | Calendar |
|---|---|
| Mainnet infra: upgrade authority → multisig, ER/sequencer infra, oracle config, insurance fund seeded | 1–2 wk |
| Deploy with **caps on** (low per-trade/position/OI, small market set, conservative leverage, kill-switch armed) | days |
| First observation window before any cap lift | 2–4 wk |

Safety = the caps + the `burn_market_authority` ladder already scaffolded.

## Phase 4 — Ramp to full production · 8–12 weeks
Widen caps stepwise as it survives real volume; add markets; lift leverage tiers; progressively burn authorities. **Deliberately slow — the slowness is the risk control.**

## Critical path & total

```
Phase 0 ███ (3–4wk, ours)
        └─▶ Phase 1 audit ██████ (6–13wk, queue-dominated)   ← book at Phase 0 start
                 ├─▶ Phase 2 bounty/chaos ████ (overlaps tail of P1)
                 └─▶ Phase 3 guarded launch ██ (2–4wk)
                          └─▶ Phase 4 ramp █████ (8–12wk)
```

| Scenario | To guarded mainnet | To full size |
|---|---|---|
| Aggressive (book audit early, single firm, fast bounty, tighter caps longer) | ~4–5 months | ~6–7 months |
| Conservative / proper (two audit perspectives, full bounty window, slow ramp) | ~7–9 months | ~10–12 months |

Engineering is only ~3–4 weeks of this. The rest is audit queue + bounty exposure + ramp observation — calendar that exists to earn down risk; compressing it is how perps DEXes get drained.

## Outside the critical path
- **Pinocchio CU cutover** (−96% CU; math layer already ported + proven): quarter-scale, mechanical, done *after* the Anchor program is live + audited, behind its own audit + ladder. Not a launch blocker.
- **Non-engineering, start day one, can gate launch independently:** legal/compliance/jurisdiction (needs real counsel) + liquidity/market-maker onboarding.

## Honest bottom line
~3–4 weeks of engineering → proven + audit-ready. **~4–5 months minimum** (realistically 7–9) to a *guarded* mainnet that has earned the right to hold real size.
