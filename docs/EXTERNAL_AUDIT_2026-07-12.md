# External-Auditor-Style Review — 2026-07-12

**Scope.** The money-path features added in this cycle — 3.1 percolator
per-domain credit (paper-profit haircut), HIP-3 permissionless market creation,
copy-vault share accounting — plus their interaction with the existing
settlement / margin / insurance system. Reviewed as an independent adversary
would: assume a hostile market creator, a hostile depositor, a compromised
sequencer, and thin/manipulated markets.

**Method.** Line-by-line read of the new code and every call site it touches;
trace of value flow through `apply_fill` settlement; check of each new authority
boundary; inventory of the formal-verification coverage; on-chain CU measurement.

## Findings

### H-1 (HIGH) — permissionless-market bad debt could leave the shared vault short. **FIXED.**

*Where.* HIP-3 isolation zeroes the insurance draw for a permissionless market
(`cover_bad_debt`, `market_is_permissionless`). But the no-haircut winner path
(`compute_realized_pnl_routing`, `delta > 0`) credits the winner the **full mark
PnL** and relies on insurance to fill a loser's shortfall. Isolating the
insurance draw without also bounding the winner credit means an uncovered
shortfall on a permissionless market becomes a **global quote-vault short** (the
winner's collateral claim exceeds the vault's tokens) — worse than the insurance
draw it replaced, because it touches every trader's withdrawals, not just the
insurance balance.

*Trigger.* A gap-through liquidation on a thin permissionless market (loss
exceeds the loser's collateral before a liquidator acts), with the haircut
engine not enabled.

*Fix.* `apply_fill` and `apply_flp_fill` now require
`!market.is_permissionless || market.haircut_enabled`. The haircut (junior-claim)
engine routes winner gains to a solvency-gated reserve that only matures while
the protocol is solvent, so an uncovered loser shortfall is absorbed by the
winner's un-matured reserve (the designed 5.2 behaviour) instead of over-crediting
the shared vault. A permissionless market therefore cannot settle a fill until
its creator enables the haircut engine — closing the vault-short window while
keeping creation permissionless. Authority markets are unaffected (they keep the
insurance backstop). Verified: SBF clean, 395 lib + 125 integ green, clippy `-D`.

### Confirmed sound (no action)

- **Copy-vaults.** The vault holds its **own isolated SPL token vault** (PDA
  authority), never the shared protocol vault — so it adds no term to global
  solvency and cannot be drawn by other markets. Shares are priced on the vault's
  **accounted** assets, not its raw token balance, so a donation to the vault ATA
  cannot inflate/deflate share price (the classic ERC-4626 first-depositor
  attack). Round-trip never extracts value (Lean `VaultShares.withdraw_le_assets`,
  `withdraw_all_returns_all`, `withdraw_mono_in_shares`). Withdrawals are bounded
  by both the share balance and the token-vault balance.
- **Percolator haircut.** `haircut_positive_pnl` scales only POSITIVE uPnL
  (losses always count in full) and is applied at both the equity and
  stress-scenario sites, so enabling it can only ever RAISE the margin
  requirement — never under-margin. Default `0` is byte-identical. The crank is
  authority/sequencer-gated (a malicious LOW value would re-open the attack, so
  the setter is trusted, exactly like the oracle/funding cranks). Lean
  `PerDomainCredit` (`usableHaircut_le_pnl`, `full_haircut_zero_credit`).
- **HIP-3 param envelope.** `validate_hip3_params` clamps leverage, floors
  maintenance, enforces `IM ≥ MM` and `IM·lev ≥ BPS`, bounds fees/shares, and
  requires a fresh oracle — Kani-proven `hip3_params_are_safe` for all inputs, so
  no hostile param combination is accepted. A permissionless creator becomes the
  sequencer but **cannot fabricate fills**: the market is armed by default and
  `apply_fill` requires a real keccak commitment from a book crossing.

## Formal-verification coverage (this cycle)

81 CI-resident Kani proofs (+5 xmargin composition, +1 HIP-3 envelope); Lean
theorems added for the percolator wired form and the copy-vault share math (both
axiom-clean, unbounded width). The division-heavy properties (haircut, vault
shares) are proven in Lean because CBMC cannot discharge symbolic non-power-of-two
division; the pure integer invariants are Kani-proven.

## Verdict

The three new features compose safely with the existing system after the H-1 fix.
No other reachable defect was found in the reviewed surface. Residual, documented
items are feature-completeness follow-ups (the off-chain ability-to-pay cranker;
the copy-vault manager-trade path; abandoned-market rent lifecycle) and the
standing external-firm audit — none is a solvency or authorization gap in the
deployed logic.
