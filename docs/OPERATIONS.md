# Operational runbooks

The remaining operational work is a deployment action, not a code migration:
execute the authority transfer under the release governance process. Every new
market initializes with the complete settlement layout, including atomic
reduce-in-flight tracking.

---

## Authority migration to a multisig

### Why

A single key currently holds the program upgrade authority and per-market
authority/sequencer roles on devnet. The on-chain governance machinery
(2-step authority transfer, 48h timelocked params updates, guardian veto,
one-way oracle-source lock, authority burn — [GOVERNANCE.md](GOVERNANCE.md))
is implemented and devnet-validated; **what remains is moving the live keys
to an M-of-N multisig.** This removes the single-key blast radius.

### Target

Squads multisig (or equivalent), recommended **3-of-5** for the upgrade
authority and market authority; a separate operational hot key for the
sequencer role only (the sequencer signs settlement, not governance — its
compromise is bounded by the commitment ring).

### Steps

```
1. Create the Squads 3-of-5 (SQUADS_MS). Record its vault PDA (SQUADS_PDA).
2. Program upgrade authority:
     solana program set-upgrade-authority 8Vdd5n4zbmxqwqY8Xv8JbEcvbih3JsEZzJBtfkoeGp2z \
       --new-upgrade-authority <SQUADS_PDA>
   Verify: solana program show <program-id> lists the multisig.
3. Per market — sequencer separation first:
     set_market_sequencer(<ops-hot-key>)        # settlement signer ≠ governance key
4. Per market — 2-step authority transfer:
     propose_authority_transfer(<SQUADS_PDA>)   # signed by current authority
     accept_authority_transfer                  # executed BY the multisig
   (The 2-step flow means a typoed key can never take authority — the
   pending target must prove it can sign.)
5. Insurance-fund + fee-tier + LP authorities: same propose/accept or
   direct-set instructions, executed to the multisig.
6. Guardian: set_guardian to a fast-response key (guardian powers are
   restrict-only, so a hot guardian key is acceptable).
7. From now on, market-params changes go through the timelocked path
   (propose_param_update → 48h → execute_param_update), leaving the
   immediate path unused; the guardian can veto a hostile proposal.
8. Optional endgame per market once params are final: lock_oracle_source,
   then burn_market_authority (both one-way).
```

### Verification

After each step, read back the on-chain field (`market.authority`,
`market.sequencer`, `insurance_fund.authority`, upgrade authority) and
confirm a probe transaction signed by the OLD key is rejected with
`Unauthorized`.

---

## 3. LP accounting: exactly one system per market

Clober carries two LP (pool-as-counterparty) accounting systems, and both
mint LP shares redeemable against the **same** protocol vault
(`insurance_fund.quote_vault`):

- **Singleton** — `LiquidityPoolAccount` (`[b"lp_exposure"]`), holding per-market
  inventory in `per_market[]`. Its realized PnL is booked automatically at
  settlement by `apply_lp_fill`; capital enters/exits via
  `lp_deposit` / `lp_withdraw`.
- **Per-market pool** — `LpMarketExposureAccount`
  (`[b"lp_per_market", market]`). Its realized PnL is booked by the
  keeper-driven `record_lp_market_fill`; capital enters/exits via
  `lp_market_deposit` / `lp_market_withdraw`.

### The constraint

**Run at most ONE of these on any given market.** There is deliberately no
on-chain interlock coupling the two (an airtight guard would require a versioned
`MarketAccount` layout field, which the account has no reserved slack for). If a
market is operated with LP capital in *both* systems and the same economic fills
are booked into both, the combined redeemable NAV
(`NAV_singleton + NAV_per_market`) can exceed the vault's true LP backing, so the last
redeemers over-withdraw and the shortfall socializes onto everyone else. This is
an operational misconfiguration, not an unprivileged exploit: both booking paths
are privileged (settlement sequencer / insurance-fund authority), and a market
that only ever uses one system is unaffected.

### Ordering, per market

1. Pick the system at market bring-up. Default to the **singleton** unless a
   per-market share ledger is specifically required.
2. Never call `lp_deposit` / `record_lp_market_fill` (native) against a market
   that already carries capital or inventory in the other system.
3. If migrating a market between systems, fully drain and zero the source
   system (no shares outstanding, no inventory) before seeding the target.

## Residual gates that are neither code nor ops runbooks

- **Professional external audit** — see [../SECURITY.md](../SECURITY.md).
- **Live volume** on the target venue.
- **LP one-system-per-market** — §3 above; an operational invariant, not a
  code interlock.
