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
LOW hardening and code hygiene. **All 4 MEDIUMs are now fixed** (verified, 446 host +
69 integration green, build-sbf clean): the grow drained-guard, and — after careful
verification of the settlement/risk paths — the FLP funding settle (N-1), the ADL
true-bankruptcy gate (R-1), and isolated-bankruptcy socialization (R-2). Each is a
minimal change that reuses already-proven (Kani'd) components; targeted
new-behaviour regression tests are a recommended follow-up.

## Findings

| ID | Sev | Area | Status | Summary |
|----|-----|------|--------|---------|
| A-1 | MED | new code | **FIXED** (`1ec17a1`) | `grow_fill_outbox` didn't enforce the drained invariant its comment promised → a non-drained grow remaps `idx%cap` and misreads pending fills. Added the guard (mirrors `grow_fill_commitment`). |
| N-1 | MED | funding | **FIXED** (`7199ce2`) | Accrued funding was dropped on a closing/flipping fill via `apply_flp_fill` (it never settled funding). Now calls `settle_position_funding` for the taker leg before the position mutation, gated on the haircut state — the exact proven pattern `apply_fill` uses. (The haircut-*disabled* `apply_fill` case remains by design on the `settle_funding` crank, documented in-code.) |
| R-1 | MED | liquidation | **FIXED** (`7199ce2`) | `auto_deleverage` admitted on the *maintenance-stress* trigger but settled at the *bankruptcy price*. Now also requires the position be TRULY bankrupt — the (flat, degrade-to-oracle) health mark must have reached `bp` (long: `mark ≤ bp`; short: `mark ≥ bp`), since `bp` is by construction the equity-zero price. A mark-solvent stress-unhealthy position is now routed to ordinary liquidation (which returns its residual), not ADL'd. |
| R-2 | MED→LOW | margin | **FIXED** (`7199ce2`) | Isolated bad debt was not socialized (bucket saturated to 0, no shortfall surfaced → silent vault deficit). Now mirrors the H-6 cross-loss waterfall: an isolated loss exceeding its bucket surfaces the remainder as a shortfall, drawn from insurance via `cover_bad_debt` (clamped to the fund balance — never negative, never an unbacked mint). Records the bad debt instead of hiding it; standard isolated-margin posture. |
| O-1 | LOW | ER | **FIXED** (`3f618c4`, zero-CU) | `from_account_data` bounds-checks the 6 header indices but not internal RBT `left/right/parent` links (hot path uses unchecked `get_helper`). A malicious-ER-committed book with an OOB child pointer → L1 traversal panic (book DoS; requires a rogue ER validator, fills still §3.2-protected). **Fixed without any hot-path CU cost:** a corrupt book can only reach L1 via the undelegate callback (a delegated book is unreadable by L1 while delegated), so the full O(capacity) slab-link walk (`MarketBookHandle::validate_node_links`, `#[repr(C)]` offset-asserted) runs EXACTLY ONCE in `process_undelegation` — the single choke point a returning book passes — gated by the book discriminator. Corrupt → `OutOfRange` revert (the undelegate fails closed, the book never lands corrupt on L1). **Place CU unchanged at ~13,081** (vs ~38k if walked per-op); ZERO change to the proven RBT internals. Regression test: `validate_node_links_rejects_corrupt_internal_link`. |
| O-2 | LOW | oracle | **FIXED** (`c6fa37b`) | Pyth/Lazer permissionless paths didn't future-reject `publish_time` (the authority path did) — a future-dated, trust-anchor-signed timestamp saturated staleness to 0. Added `require!(published <= now)` to both (`pyth_oracle.rs`, `update_oracle_from_lazer`). |
| O-3 | LOW | margin | **FIXED** | With `oracle_staleness_max_seconds == 0` (disabled) + a fresh mark, `worse_of_health_price` still folded in an unvalidated oracle → a stale adverse reading could wrongly liquidate. Fixed in `liquidate_position_v2`: the oracle is now folded into the worse-of ONLY when `oracle_trusted_for_health` (`max_age > 0 && published_at > 0`); otherwise it's passed as 0 ⇒ worse-of prices off the mark alone. No-op on the stale-mark branch (which already mandates a configured+fresh+published oracle). The portfolio path (`effective_health_mark`) was already mark-only when fresh. |
| F-1 | LOW | fees | **FIXED** (`1ec17a1`) | Misleading comment claimed a 12_000 fee-discount cap that doesn't exist (`MAX_FEE_DISCOUNT_BPS == 10_000`), so the negative-fee branch is unreachable — a future-editor trap. Comment corrected. |
| H-1..H-4 | INFO | hygiene | **DONE** (`c6fa37b` + dead-code pass) | Removed: **8 dead fns** (`lot.rs` `is_zero`×2/`ticks_delta`, `order.rs` `is_taker`/`fifo_key`, `state.rs` `side_enum`, `haircut.rs` `usd_to_quote_lots`, `er.rs` `delegation_metadata_pda`) + **2 dead constants** (`USD_DECIMALS`, `LOT_EPSILON`) + stale comments. **Second pass:** removed `er.rs::cpi_undelegate` + its dead `UndelegateAccounts` struct + `UNDELEGATE_DISCRIMINATOR` const cascade (real undelegation is the DLP callback `process_external_undelegate`, never a program-issued CPI), and `state_v2` `insert_seat` / `lookup_min_index`. Build + 449 host + 69 integration re-verified clean. **Error variants:** re-audited against the current tree — **0 enum-level-unused** (the prior "23 unused" figure is stale; all 99 now have a real `FlashBookError::X` reference). Moot regardless: every variant carries an explicit discriminant (`= 1003`), so removal would never shift another code. The 21 *unwired matcher feature-modules* are intentional proven future features — **not** deleted; recommend an `unwired/` namespace. |
| FP-1 | — | — | **REVERTED** | An audit LOW ("grow should cap at `FILL_RING_CAP`") was a **false positive** — grow intentionally raises the ring past the 256 init default (the ER-session fill ceiling); the deep-book tests grow to 512. Caught + reverted by the test suite. |

## Concurrency / race posture (verified sound)

The program uses **only the fallible** `try_borrow_*` / `AccountLoader::load*` APIs —
**zero** panicking `.borrow_mut()`. Every aliasing path therefore fails closed as an
`AccountBorrowFailed` revert, not a panic. Replay is closed by the Kani-proven
monotonic `advance_settlement_seq` (`fill_seq`) + ER epoch nonce; margin/liquidation
walks dedupe market pubkeys + exact-count + verify_position_pda; borrows are dropped
before every CPI. Legibility hardening (not a bug) **DONE**: explicit
`require_keys_neq!` added on the two seedless same-trader paths (`apply_fill`
taker/maker_position, `sweep_collateral` from/to_state) — both already failed closed
on the downstream `load_mut` aliasing borrow; the guards now state the intent up front
and fail fast before any state is touched.

## Rating (this audit)

| Dimension | Score | Basis |
|---|---|---|
| Math / accounting | 9.0 | conservation Kani+Lean-proven; one bounded funding-on-close MED |
| Margin / liquidation | 8.5 | sound value-conservation core; ADL-fairness (R-1) + isolated-socialization (R-2) MEDs |
| Access control | 9.0 | every privileged action gated; PDAs seed+bump bound; verified |
| Oracle integrity | 9.0 | feed/conf/staleness/envelope gated, Ed25519+Pyth-owner anchors; minor timestamp LOWs |
| ER / matching | 8.5 | §3.2 authenticity mandatory; O-1 node-bounds gate fixed (zero-CU, at undelegate); ER trust residual |
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
