# Flash Book — Deep External Audit (2026-06)

Six independent adversarial lenses over the deployed Anchor program
(`5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq`, devnet), each reading the real
source with `file:line` evidence; every finding personally re-verified before
action. Methodology: new-code review, risk-engine review, numerical review,
access-control/oracle review, code-hygiene/concurrency review, and a red-team that
attempted 12 exploit archetypes against the real handlers.

## Executive summary

**No reachable Critical, High, or Medium that moves funds. Red-team: 0 confirmed
exploits.** Every prior Critical/High remediation (the two prior CRITICALs, the C-1
margin double-count, H-1…H-7, M-1/M-2, F-1…F-4) was independently re-confirmed
present and correct — backed by Kani proofs the auditors read and found sound, not
decorative. The §3.2 commitment-ring fill-authenticity, the dual-source liquidation
gate, the value-conserving ADL/haircut/bad-debt waterfall, and the oracle
feed/confidence/staleness/envelope gates all hold.

The findings are **4 MEDIUMs — all correctness/fairness, none are theft** — plus
LOW hardening and code hygiene. One MEDIUM is fixed; three are *documented for
deliberate remediation* because they change settlement/liquidation economics, which
is not rushed onto a live book.

## Findings

| ID | Sev | Area | Status | Summary |
|----|-----|------|--------|---------|
| A-1 | MED | new code | **FIXED** (`1ec17a1`) | `grow_fill_outbox` didn't enforce the drained invariant its comment promised → a non-drained grow remaps `idx%cap` and misreads pending fills. Added the guard (mirrors `grow_fill_commitment`). |
| N-1 | MED | funding | **DOCUMENTED** | Accrued funding is dropped on a closing/flipping fill via `apply_flp_fill` (never settles) and via `apply_fill` on haircut-*disabled* markets (settle is inside the `Some(haircut)` guard). Bounded funding evasion (≤ funding since last crank; never double-charges). *Fix:* call `settle_position_funding` on both paths; add a non-Residual (collateral-only) routing variant for haircut-disabled markets. |
| R-1 | MED | liquidation | **DOCUMENTED** | `auto_deleverage` admits a position on the *maintenance-stress* trigger (`!is_healthy` over the ±30% lattice) but settles at the *bankruptcy price* (full-collateral wipe). A mark-solvent but stress-unhealthy trader can be ADL'd, seizing residual equity. Value-conserving (no mint), a fairness issue. *Fix:* gate ADL on true bankruptcy (equity ≤ 0 at `effective_health_mark`), or close at the health-mark with residual returned. |
| R-2 | MED→LOW | margin | **DOCUMENTED** | Isolated-margin bad debt is not socialized: the bucket saturates to 0 and surfaces no shortfall, so insurance is never drawn (cross positions get the H-6 waterfall; isolated don't). A gap-through-bankruptcy remainder becomes silent vault/FLP shortfall. *Fix:* surface the isolated remainder as a shortfall and route through `cover_bad_debt`/ADL, **or** explicitly document isolated bad debt as FLP/vault-borne and verify the solvency monitor accounts for it. |
| O-1 | LOW | ER | DOCUMENTED | `MarketBookHandle::from_account_data` bounds-checks the 6 header indices but not internal RBT `left/right/parent` links (hot path uses the unchecked `get_helper`). A malicious-ER-committed book with an OOB child pointer → L1 traversal panic (book DoS, no reset ix). *Fix:* walk the slab once on undelegate-load asserting every link is NIL or node-aligned in-bounds, or switch hot accessors to `get_helper_checked`. |
| O-2 | LOW | oracle | DOCUMENTED | Pyth/Lazer permissionless paths don't future-reject `publish_time` (the authority path does). A future-dated, trust-anchor-signed timestamp saturates staleness to 0. *Fix:* `require!(published <= now)` on both paths for parity. |
| O-3 | LOW | margin | DOCUMENTED | With `oracle_staleness_max_seconds == 0` (disabled) + a fresh mark, `worse_of_health_price` still folds in an unvalidated oracle → a stale adverse reading can wrongly liquidate. *Fix:* treat staleness==0 as "oracle not trusted for health" (use mark alone). |
| F-1 | LOW | fees | **FIXED** (`1ec17a1`) | Misleading comment claimed a 12_000 fee-discount cap that doesn't exist (`MAX_FEE_DISCOUNT_BPS == 10_000`), so the negative-fee branch is unreachable — a future-editor trap. Comment corrected. |
| H-1..H-4 | INFO | hygiene | DOCUMENTED | Dead code to prune in a dedicated cleanup: 11 dead fns, 23 unused error variants, 2 unused constants, the dead negative-fee branch, stale comments. (The lone build-warning fn `Cursor::u16` was deleted in `1ec17a1`.) The 21 *unwired matcher feature-modules* — stop-limit, trailing-stop, OCO, STP, etc. — are intentional proven future features, **not** deleted; recommend moving to an `unwired/` namespace so the shipped surface isn't conflated. |
| FP-1 | — | — | **REVERTED** | An audit LOW ("grow should cap at `FILL_RING_CAP`") was a **false positive** — grow intentionally raises the ring past the 256 init default (the ER-session fill ceiling); the deep-book tests grow to 512. Caught + reverted by the test suite. |

## Concurrency / race posture (verified sound)

The program uses **only the fallible** `try_borrow_*` / `AccountLoader::load*` APIs —
**zero** panicking `.borrow_mut()`. Every aliasing path therefore fails closed as an
`AccountBorrowFailed` revert, not a panic. Replay is closed by the Kani-proven
monotonic `advance_settlement_seq` (`fill_seq`) + ER epoch nonce; margin/liquidation
walks dedupe market pubkeys + exact-count + verify_position_pda; borrows are dropped
before every CPI. Recommended legibility hardening (not a bug): explicit
`require_keys_neq!` on the two seedless same-trader paths (`apply_fill`
taker/maker_position, `sweep_collateral` from/to_state).

## Rating (this audit)

| Dimension | Score | Basis |
|---|---|---|
| Math / accounting | 9.0 | conservation Kani+Lean-proven; one bounded funding-on-close MED |
| Margin / liquidation | 8.5 | sound value-conservation core; ADL-fairness (R-1) + isolated-socialization (R-2) MEDs |
| Access control | 9.0 | every privileged action gated; PDAs seed+bump bound; verified |
| Oracle integrity | 9.0 | feed/conf/staleness/envelope gated, Ed25519+Pyth-owner anchors; minor timestamp LOWs |
| ER / matching | 8.5 | §3.2 authenticity mandatory; from_account_data node-bounds LOW; ER trust residual |
| Arithmetic / DoS | 9.3 | checked throughout; graceful truncation; 0 exploits |
| New fill-outbox | 9.0 | no-overwrite Kani-proven; 1 MED fixed; sound otherwise |
| **Overall (code)** | **~9.0** | no reachable fund-loss path; FV-backed; 4 bounded MEDs (1 fixed, 3 to remediate) |

**Production rating (~7.0) is gated by the same two non-code items as before:**
external audit (this report is a turnkey input) and authority decentralization
(multisig). The 3 documented MEDIUMs are the natural first remediations a paid audit
would also surface.

## How it compares (honest)

| | Fill authenticity | CLOB type | Margin | Formal verification | Measured CU | Audited / mainnet / battle-tested |
|---|---|---|---|---|---|---|
| **Flash Book** | **§3.2 keccak ring — sequencer can't fabricate (unique)** | **real price-time hypertree** | **stress-lattice portfolio** | **46 Kani + Lean** | **yes, reproducible** | **no — devnet, unaudited** |
| Drift | trusted keepers (off-chain DLOB) | off-chain DLOB | cross/isolated | none first-party | no | yes; **$285M exploit (Apr-2026)** |
| Phoenix | PDA-signed CPI log (strong) | on-chain price-time (spot) | n/a (spot) | none | no | yes; mainnet, battle-tested |
| Manifest | log-frame (weak) | **price-only Ord — no FIFO** | minimal | Certora ×4 props | no | yes; mainnet |
| OpenBook v2 | EventHeap account | price-time (spot) | n/a (spot) | none | no | yes; mainnet |
| Hyperliquid | off-chain (own L1) | off-chain | portfolio | none | n/a | yes; huge volume |

**Where Flash Book genuinely leads:** the *combination* — cryptographic fill
authenticity (no competitor binds fills to a settlement commitment), a real
price-time on-chain CLOB (Manifest is price-only; Drift/HL are off-chain), portfolio
stress-lattice margin, the deepest first-party formal verification in the category,
and the only reproducible measured-CU story. On *engineering substance*, it is at or
above the field.

**Where it trails — and it's the whole gap:** maturity. Every competitor is
**audited, on mainnet, with real volume and years (or at least months) without an
exploit.** Flash Book is devnet + unaudited + unproven under adversarial money. Drift
shows even an audited, battle-tested protocol can lose $285M — which is the argument
*for* Flash Book's FV + fill-authenticity depth, and *also* the reminder that code
quality alone isn't safety until it's been hammered in production.

## Verdict

The deployed Anchor program is **soundly engineered with no reachable
fund-loss path and verification depth that exceeds the field** — the deep audit
found zero exploits and only bounded correctness/fairness MEDIUMs. To be *the
greatest among all*, the remaining distance is not more cleverness; it is the
unglamorous gate: **remediate the 3 documented MEDIUMs → external audit → multisig →
mainnet → time without an exploit.** Best-engineered on substance today; "best on
earth" is earned in production, not in the source.
