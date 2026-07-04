# Governance hardening — design spec

Status: **SPEC — design only, not implemented.** Author: audit-remediation follow-up, 2026-07.

Goal: remove the single-key blast radius that is, on its own, a $1B-custody blocker regardless of
the correctness fixes (F-1/F-2/F-4). This is a mix of **ops** decisions (put keys behind a
multisig) and **on-chain code** (timelock, 2-step transfers, one-way locks). Each phase is scoped
so it can ship and be devnet-validated independently.

---

## 1 · Threat model — the current blast radius (grounded)

Three single keys, no timelock, no multisig, no on-chain proposal delay:

| Key | Controls (examples) | Worst case if the single key is lost/malicious |
|---|---|---|
| **Program upgrade authority** (BPF loader; currently one wallet, `solana program show` → `Authority`) | Replace the *entire program* instantly | Total: swap in code that drains every vault |
| **`market.authority`** (per market; **64 authority-gated ixs** guard on `market.authority == authority.key()`) | `update_market_params`, `update_oracle` (direct price write — H-6 caps the per-slot *move* but the authority can still walk price within the cap every slot → drive worse-of liquidations), `set_market_status` (pause/close), `delegate_*`, `grow_fill_commitment`, oracle config | Wrong-config a market, walk the mark, brick/close it |
| **`insurance_fund.authority`** | `withdraw_insurance_fund` (`lib.rs:2740`) | Drain the insurance fund |

No `multisig` / `timelock` / `governance` account exists today (grep: only *comments* deferring to
"a future governance field once `MarketParams` is versioned"). `set_market_status` (`lib.rs:5740`)
is single-key and **symmetric** — the same key pauses *and* unpauses, so a compromised key can
un-pause right back.

## 2 · Design principles

- **Asymmetry.** Fail-safe actions (pause, tighten margin, lock oracle source) should be **fast**
  (guardian / low threshold). Fail-dangerous actions (unpause, loosen margin, transfer authority,
  upgrade, insurance withdraw) should be **slow** (timelock) and **high threshold**.
- **No new trust in the hot path.** Governance changes must not add accounts or CU to
  place/match/settle. All of this lives on admin ixs only.
- **Layout-safe → use SEPARATE PDAs, not `MarketAccount` fields.** New governance state gets its own
  small PDA (the Phase-1 `MarketGuardianAccount` pattern: seeds `[b"<name>", market]`, optional in
  the consuming context). **Do NOT add trailing fields to `MarketAccount`.** Empirically confirmed
  during Phase 1: `MarketAccount` is held UNBOXED (`Account<MarketAccount>`, ~1152 B) in dozens of
  `try_accounts` frames, several already within a few bytes of the 4 KB BPF stack limit
  (`ViewMarket` 4104, `InitFlpPerMarketV3` 4160). Adding a single 32-byte `Pubkey` field pushed
  `SettleFunding` and `VaultPlaceOrderV3` past 4096 with **"overwrites frame / undefined behavior"**
  errors. `space()=1152` has byte headroom, but the *stack* does not — the binding constraint is the
  4 KB frame, not the account size. So the M-6 `unsettled_fill_volume` trailing-field trick is a
  one-off that must NOT be repeated for governance. A separate PDA adds bytes only to the one admin
  context that reads it (far from the limit) and touches no hot path.
- **Two-step, never fat-finger.** Authority/ownership transfers are propose→accept, so a typo can't
  strand control at a dead key.

## 3 · Phased plan

### Phase 0 — Ops only, no code (ship immediately)
- **Move the program upgrade authority to a Squads multisig** (M-of-N) and, once available, behind
  Squads' timelock. Verifiable on-chain: `solana program show <pid>` → `Authority` == the multisig
  PDA. This alone removes the single largest blast radius with zero program change.
- **Move `market.authority` and `insurance_fund.authority` to the same (or a separate ops)
  multisig** by calling the existing authority-set paths with the multisig as the new authority.
  (If no set-authority ix exists for one of them, that's Phase 2's 2-step transfer.)

### Phase 1 — Asymmetric pause (small, on-chain) — ✅ SHIPPED (commits `59a9fba`, `dee2133`)
- **`MarketGuardianAccount` PDA** (seeds `[b"market_guardian", market]`; `{market, guardian, bump}`)
  — a SEPARATE account, NOT a MarketAccount field (see the Layout-safe principle; the field version
  blew the 4 KB stack). Set/cleared by the authority via `set_guardian` (init_if_needed).
- `set_market_status` moved to its own `SetMarketStatus` context (so the optional guardian account
  does not leak into the 4 other handlers sharing `UpdateMarketAuthority`) and now accepts EITHER
  `market.authority` OR the guardian:
  - guardian may move only to a STRICTLY more-restricted live status (PostOnly/Paused/Closed,
    monotonic on the enum values `Active 1 < PostOnly 2 < Paused 3 < Closed 4`, from a live state);
  - any loosening (→ Active/Inactive) or lifecycle change stays authority-only.
- Validated: host + BanksClient (guardian pauses / can't unpause / rando rejected / authority
  re-opens / `set_guardian` authority-only) AND live on devnet (acceptance 18/18).
- NOTE for Phase 2: "unpause routes through the timelock" is deferred to Phase 2 — today unpause is
  authority-only (already the fail-dangerous key, just not yet delayed). Wiring unpause through
  `PendingGovAction` is a Phase-2 follow-up.

### Phase 2 — On-chain timelock + 2-step transfers (the core)
Shipped as two focused, self-contained pieces (each its own PDA — the separate-PDA principle —
rather than one generic `PendingGovAction`, which was deferred as a larger dispatch framework):

**Phase 2a — 2-step authority transfer — ✅ SHIPPED (commits `0618e41`, `629ae80`; live 23/23).**
`MarketPendingAuthorityAccount` PDA (`[b"pending_authority", market]`). `propose_authority_transfer`
(authority) stores the pending key; `accept_authority_transfer` — signed BY that new key — commits
`market.authority` and closes the pending account; `cancel_authority_transfer` (authority) revokes.
Because the new key must sign to accept, control can never strand at a mistyped/dead key (the 1-step
`transfer_market_authority` failure mode); that immediate path is retained.

**Phase 2b — timelocked market-params update — ✅ SHIPPED (commits `a45853b`, `66eeb67`; live 26/26).**
`PendingParamUpdateAccount` PDA (`[b"pending_params", market]`). `propose_param_update` validates the
new params (shared `validate_market_params`, identical to the immediate `update_market_params`) and
stores `keccak(params) + eta = now + PARAM_UPDATE_TIMELOCK_SECONDS` (48h); it does NOT apply.
`execute_param_update` applies only when `now >= eta` AND `keccak(supplied) == stored` (the change is
exactly the pre-announced one), re-validates, and closes the pending. `cancel_param_update`
(authority) revokes. `ParamUpdateProposedEvent` carries `eta` for off-chain alerting. The immediate
`update_market_params` is retained as the un-timelocked path. **Follow-up (ready, not shipped):**
`guardian_veto_param_update` — let the emergency guardian cancel a pending param change during the
delay (fail-safe emergency brake), as a pure-addition ix that doesn't touch the authority cancel path.

### Phase 3 — Deprecate direct-write `update_oracle` on production markets — ✅ SHIPPED (commit `f9cf4ad`; live 29/29)
`lock_oracle_source` (authority-only, ONE-WAY — no unlock ix). Once locked, the direct-authority
`update_oracle` / `update_oracle_quorum` paths **revert** (`OracleSourceLocked`) — only the Pyth
(account-owner + `VerificationLevel::Full`) and Lazer (Ed25519 precompile + replay nonce) paths
remain, removing the "authority walks the mark within the H-6 per-slot cap" vector on locked markets.

**Design correction (important — the original spec here was BYPASSABLE):** an *optional* lock account
can be omitted by the caller, so a lock stored in one is not a control. The shipped design instead
carves a **`source_locked: u8`** flag from the `MarketEnvelopeConfigAccount`'s `_reserved` bytes
(size-unchanged; pre-existing envelopes read `0`) — i.e. the account the two direct paths **already
REQUIRE** (H-6). So the check is **un-bypassable** (can't omit the account), adds **no new account**
to the hot `update_oracle` context (no ABI break, no test churn), and Pyth/Lazer (which don't get the
check) keep working. The flag is set only on first-init (open) and by `lock_oracle_source` (locked);
`set_envelope_config` re-config never touches it, so the lock cannot be reset.

## 4 · What this does NOT do (explicit residuals)
- It does not decentralize the **ER sequencer** (separate project — the true Hyperliquid/dYdX
  differentiator; today a documented single-sequencer trust boundary).
- It does not remove trust in the multisig signers themselves — it raises the bar from 1 key to
  M-of-N + delay, which is the industry-standard mainnet posture, not trustlessness.
- Timelock protects against a *compromised* key; it cannot protect against an M-of-N *collusion*.

## 5 · Verification plan (per phase)
- Phase 0: on-chain `solana program show` + account `authority` reads == multisig PDA; a direct
  (non-multisig) admin ix now fails `Unauthorized`.
- Phase 1: BanksClient — guardian pauses (ok) / guardian unpause (reverts) / authority tighten (ok).
- Phase 2: BanksClient — `execute` before `eta` reverts (TimelockNotElapsed); after `eta` applies;
  payload mismatch reverts; 2-step transfer requires the *new* key to accept; cancel works.
- Phase 3: BanksClient — after lock, direct `update_oracle` reverts; Pyth/Lazer paths still accept.
- Layout/stack: governance state lives in **separate PDAs**, so `MarketAccount` is unchanged — but
  after ANY governance change, re-run `cargo build-sbf --tools-version v1.52` and confirm the stack
  warning set is unchanged (baseline: `ViewMarket`, `InitFlpPerMarketV3` only). A new context that
  holds `MarketAccount` unboxed plus another sizeable account can itself cross 4096 — Box the
  `MarketAccount` (transparent deref) or split the context if so. Never grow `MarketAccount`.

## 6 · Cost / sequencing
Phase 0 is hours (ops). Phase 1 is a small on-chain change. Phase 2 is the real work (new PDA +
3–4 ixs + tests + devnet). Phase 3 is small but depends on Phase 2's timelock. Ship 0→1→2→3; each is
independently devnet-validatable and none touches the hot settlement path.
