# Operational runbooks

Two operational steps stand between the current code and a
production-hardened deployment. **Both are deployment/ops actions, not code
changes** — the on-chain support for each is implemented, tested, and
proven; what remains is executing the migration on live markets and keys.

---

## 1. Per-market fill-commitment v1 upgrade (full reduce-only closure)

### Why

Reduce-only orders are enforced in two tiers
([SETTLEMENT.md](SETTLEMENT.md) §3):

- **v0 (every market, default):** the injection-time capacity clamp closes
  the primary over-reduce vector at match time, plus a TTL on injected
  close orders.
- **v1 (opt-in per market):** the fill-commitment ring's v1 layout adds
  per-position reduce-in-flight tracking co-located with the ring, so the
  tracker commits atomically with settlement. This closes the last edge:
  a position shrunk below its resting reduce-only size, then over-crossed
  across the match→settle gap.

A v0 market is safe against the primary vector; **full closure requires the
one-time v1 upgrade on each live market.**

### The instruction

`upgrade_fill_commitment_v1` — market-authority-gated, one-way (a v1 ring
rejects re-upgrade). Hard preconditions, all enforced on-chain
(fail-closed):

1. **Base layer, undelegated ring.** A delegated ring is owned by the
   delegation program; realloc would be illegal. Undelegate first.
2. **Drained ring** (`produced == settled`). A pending fill would be left
   without a reduce flag; the upgrade rejects with `FillRingNotDrained`.
3. The authority tops up rent for the enlarged account automatically
   (part of the instruction; keep a small SOL balance on the authority).

### Ordering, per market

```
1. Quiesce:      pause new taker flow at the sequencer (or set_market_status
                 to a restricted mode) and let apply_fill drain the ring
                 until produced == settled.
2. Undelegate:   commit_and_undelegate_market_book +
                 commit_and_undelegate_fill_commitment (+ outbox) on the ER;
                 process_undelegation finalizes on L1.
3. Upgrade:      upgrade_fill_commitment_v1 (authority signs).
4. Verify:       fetch the ring account; version byte == 1, cap unchanged,
                 produced == settled, account length ==
                 fill_commit_account_len_v1(cap). A FillCommitmentUpgradedEvent
                 is emitted with the old/new byte sizes.
5. Re-delegate:  delegate_market_book + delegate_fill_commitment
                 (+ delegate_fill_outbox) together, pinned to the ER validator.
6. Probe:        place + settle one reduce-only round-trip; confirm the
                 in-flight counter increments at match and releases at
                 settlement (integration suite: the v1-ring settle tests).
7. Resume traffic.
```

Steps 1–2 and 5 are the same quiesce/undelegate/re-delegate sequence as
arming a ring ([ER_TRUST_BOUNDARY.md](../ER_TRUST_BOUNDARY.md) §4); the
upgrade itself is one transaction per market.

### Rollback

None needed or possible: the upgrade is one-way by design, v0 accounts are
byte-identical until upgraded, and every precondition failure leaves the
ring untouched.

---

## 2. Authority migration to a multisig

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
     solana program set-upgrade-authority 5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq \
       --new-upgrade-authority <SQUADS_PDA>
   Verify: solana program show <program-id> lists the multisig.
3. Per market — sequencer separation first:
     set_market_sequencer(<ops-hot-key>)        # settlement signer ≠ governance key
4. Per market — 2-step authority transfer:
     propose_authority_transfer(<SQUADS_PDA>)   # signed by current authority
     accept_authority_transfer                  # executed BY the multisig
   (The 2-step flow means a typoed key can never take authority — the
   pending target must prove it can sign.)
5. Insurance-fund + fee-tier + FLP authorities: same propose/accept or
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

## 3. FLP accounting: exactly one system per market

Flash Book carries two FLP (pool-as-counterparty) accounting systems, and both
mint LP shares redeemable against the **same** protocol vault
(`insurance_fund.quote_vault`):

- **Singleton** — `FlpExposureAccount` (`[b"flp_exposure"]`), holding per-market
  inventory in `per_market[]`. Its realized PnL is booked automatically at
  settlement by `apply_flp_fill`; capital enters/exits via
  `deposit_flp_capital` / `withdraw_flp_capital`.
- **Per-market v3** — `FlpExposurePerMarketAccountV3`
  (`[b"flp_per_market", market]`). Its realized PnL is booked by the
  keeper-driven `record_flp_fill_v3`; capital enters/exits via
  `flp_deposit_v3` / `flp_withdraw_v3`.

### The constraint

**Run at most ONE of these on any given market.** There is deliberately no
on-chain interlock coupling the two (an airtight guard would require a versioned
`MarketAccount` layout field, which the account has no reserved slack for). If a
market is operated with LP capital in *both* systems and the same economic fills
are booked into both, the combined redeemable NAV
(`NAV_singleton + NAV_v3`) can exceed the vault's true FLP backing, so the last
redeemers over-withdraw and the shortfall socializes onto everyone else. This is
an operational misconfiguration, not an unprivileged exploit: both booking paths
are privileged (settlement sequencer / insurance-fund authority), and a market
that only ever uses one system is unaffected.

### Ordering, per market

1. Pick the system at market bring-up. Default to the **singleton** unless a
   per-market share ledger is specifically required.
2. Never call `deposit_flp_capital` / `record_flp_fill_v3` (v3) against a market
   that already carries capital or inventory in the other system.
3. If migrating a market between systems, fully drain and zero the source
   system (no shares outstanding, no inventory) before seeding the target.

## Residual gates that are neither code nor ops runbooks

- **Professional external audit** — see [../SECURITY.md](../SECURITY.md).
- **Live volume** on the target venue.
- **FLP one-system-per-market** — §3 above; an operational invariant, not a
  code interlock.
