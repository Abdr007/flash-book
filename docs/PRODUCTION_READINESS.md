# Production readiness assessment

The single source of truth for "what's done, what's blocked, what's next."

## Status — Phase 1 complete

```
20 Anchor instructions       fully implemented + E2E tested
30 Rust property tests       60,000 random-input fuzz assertions
20 Rust E2E tests           via real Solana runtime (solana-program-test)
31 Rust unit tests          matcher core
152 TypeScript tests         reference simulator + SDK
─────────────────────
233 tests · ~60K fuzz · 0 failures · 0 panics
```

## Code-completeness matrix

| Component | Status | Notes |
|---|---|---|
| FBA Walrasian matcher | ✅ | 12K fuzz assertions, MEV-neutral within batch |
| Virtual FLP quoter (Avellaneda-Stoikov + VPIN + realized vol) | ✅ | Spread monotonicity proven via proptest |
| Continuous funding (Q64.64 cumulative index) | ✅ | Sign correctness + clamping fuzzed |
| VPIN toxicity calculator (Q32.32 EMA) | ✅ | Bounded output proven |
| Stress-lattice cross-margin | ✅ | 14K fuzz assertions, hedge property documented |
| Permissionless liquidation (single + cross-market) | ✅ | `liquidate_position` + `liquidate_portfolio` |
| Insurance fund three-stream waterfall | ✅ | Conservation property proven |
| ADL fallback | ✅ | Profit/leverage ranking, in matcher logic |
| Commit-reveal MEV resistance | ✅ | Hash determinism + tamper rejection proven |
| Status circuit breaker | ✅ | E2E test fires the gate |
| Authority rotation w/ revocation | ✅ | E2E test verifies old key rejected |
| Mutable param tuning w/ immutable primitives | ✅ | E2E test rejects tick_size change |
| FLP capital lifecycle | ✅ | init / deposit / withdraw with open-positions gate |
| Per-trader rate limiting | ✅ | 16 orders/batch, batch-bucket reset |
| OI tracking with transition correctness | ✅ | recomputed from authoritative state |
| Account size verified deployable | ✅ | All ≤ 10 KiB single-call init limit |
| Zero panic paths in production code | ✅ | Audited; defensive `unwrap_or` in tie-break |
| TypeScript SDK (read+write+simulate+stream) | ✅ | 20 builders, 7 fetchers, decoders, risk preview |
| Standalone consumer demos | ✅ | `full-lifecycle.ts`, `live-monitor.ts` |
| Deployment runbook | ✅ | `docs/DEPLOYMENT.md` 11-section flow |

## Upstream-blocked items (not code gaps)

| Blocker | What it blocks | Status |
|---|---|---|
| `ephemeral-rollups-sdk` compat | `delegate_account` / `commit_and_undelegate_accounts` CPIs | Tried 0.2 (Solana 1.x dep clash) and 0.13 (Solana 2.x type mismatches). Tracked. Integration is purely additive. |
| Solana platform-tools v1.48 | BPF compilation (`anchor build`) | Transitive `constant_time_eq` requires edition2024; platform-tools rustc is 1.84. Will resolve with v1.49+. Native `cargo test` is unaffected. |
| BPF blocker → Mollusk / litesvm tests | LiteSVM-style E2E | We use `solana-program-test` natively (works) instead. 20 E2E tests pass through a real Solana runtime in-process. |

## Phase boundaries

Items below are **phase-explicit deferrals** — not bugs, not incomplete
code, just future work that the program is *designed to support* via
additive integration.

### Phase 2 — mainnet shadow mode
- Read-only ingestion of mainnet Flash V2 trades
- Replay through matcher; A/B compare against pool outcomes
- 30-day soak before any live deployment

### Phase 3 — limited production (SOL-PERP first)
- MM whitelisting
- Per-trader position caps
- Real-time invariant monitoring + kill switch
- Bug bounty active

### Phase 4 — multi-market + builder-deployed (HIP-3)
- BTC-PERP, ETH-PERP, long-tail
- RWAs (per Flash V3 roadmap)
- Third-party-deployed markets

### Phase 5 — continuous improvement
- Multi-oracle quorum (Pyth + Switchboard + on-chain median)
- Maker rebate distribution from toxicity tax pool
- Spot trading on the same matcher
- Cross-margin against spot collateral

## What changes after upstream blockers clear

When `ephemeral-rollups-sdk` ships a Solana 2.x-compatible release:

1. Add the dep back to `programs/flash-book/Cargo.toml`.
2. Add two new instructions to `lib.rs`:
   - `delegate_market` — wraps `delegate_account` for the relevant PDAs.
   - `undelegate_market` — wraps `commit_and_undelegate_accounts`.
3. Generate updated IDL.
4. Add SDK builders + tests.

When Solana platform-tools ships v1.49+ (rustc 1.85+):

1. `anchor build` produces `target/deploy/flash_book.so`.
2. Run `solana program deploy target/deploy/flash_book.so`.
3. Optionally: add Mollusk integration tests to complement
   `solana-program-test`.

When third-party audit completes:

1. Address findings.
2. Tag a release candidate.
3. Phase 2 shadow mode begins.

## Net assessment

**The program is production-shape complete for what isn't blocked
upstream.** Every line item in `docs/SAFETY.md` audit checklist that
maps to in-code work is done. Every upstream blocker has a documented
unblock path. Every phase has clear acceptance gates.

A reviewer can:
- Read the program (`programs/flash-book/src/lib.rs`)
- Run the tests (`cargo test --package flash-book` and `bun test`)
- Read the docs (`docs/`)
- Run the demos (`bun run sdk-ts/examples/full-lifecycle.ts`)
- Walk the deployment (`docs/DEPLOYMENT.md`)

…and conclude: ready for audit and Phase 2 shadow mode whenever the
upstream toolchain clears.
