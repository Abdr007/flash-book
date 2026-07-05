# Incident Response Playbook

What to do when Flash Book misbehaves on mainnet. Each scenario lists
detection signals, immediate actions, root-cause investigation, and
post-incident steps.

## Severity classification

| Level | Definition | Response time |
|---|---|---|
| **P0** | Loss of funds, protocol insolvency, vault drain | < 5 minutes |
| **P1** | Liquidations failing, no oracle updates, mass position halt | < 30 minutes |
| **P2** | Single market degraded, keeper lag, fee mis-routing | < 2 hours |
| **P3** | Cosmetic / non-financial | next business day |

---

## P0 scenarios

### 1. Vault balance < accounting expectation

**Detection**: `verify_protocol_solvency` ix returns
`HaircutResidualUnderflow` OR `ProtocolSolvencyCheckedEvent.solvent =
false`.

**Immediate**:
1. Authority calls `set_market_status(Paused)` on every market.
2. Authority calls `set_market_status(Paused)` on FLP via market
   pause (FLP withdrawals already block on per-position checks).
3. Notify users via official channels. Halt new deposits.

**Investigate**: pull `FillAppliedEvent`, `CollateralDepositedEvent`,
`CollateralWithdrawnEvent`, `FundingSettledEvent`,
`FlpCapitalUpdatedEvent`, `InsuranceContributionEvent`,
`PositionConvertedEvent` from the past N hours. Reconcile:

```
expected_vault = Σ deposits − Σ withdrawals
                + Σ insurance_contribs − Σ insurance_payouts
                + Σ flp_deposits − Σ flp_withdrawals
                + Σ fees_collected
```

vs `quote_vault.amount` on-chain.

**Recovery**: depends on root cause. Common causes:
- SPL transfer succeeded but accounting failed → reconcile via
  `seed_residual` (authority op) or manual collateral correction.
- Bug in PnL math → freeze, patch, redeploy with state migration.

### 2. Insurance fund drained below pause threshold

**Detection**: `InsuranceContributionEvent.balance_quote_lots ≤
pause_threshold_quote_lots` for the relevant market params.

**Immediate**:
1. Authority calls `set_market_status(PostOnly)` on every market with
   open positions on the insolvent side.
2. Trigger `auto_deleverage` permissionlessly until insurance recovers.

**Investigate**: which liquidations consumed insurance? Pull
`LiquidationInjectedEvent` + `InsurancePayoutEvent`. Identify the
underlying positions and their losses.

### 3. Wrong oracle price drives wrong liquidations

**Detection**: liquidation rate spike + `OracleUpdatedFromPythEvent`
showing extreme prices. Cross-reference Pyth web UI vs on-chain.

**Immediate**:
1. Authority calls `set_market_status(Paused)` on affected market.
2. Trigger envelope-config update with tighter `max_price_move_bps`
   to prevent further extreme moves landing.

**Investigate**:
- Was Pyth feed correct? Compare on-chain `oracle_price_ticks` against
  Pyth UI.
- Did envelope gate fail? Check `gate_passes` / `gate_rejects` on
  `MarketEnvelopeConfig`.
- Did multiple sources agree? If quorum oracle, check dispersion event
  history.

**Recovery**: replay liquidations against corrected oracle, refund
victims from insurance, prosecute keepers who profited.

---

## P1 scenarios

### 4. Oracle stops updating

**Detection**: `liquidate_position_v2` returning `OracleTooStale` >
30 min. Pyth Solana publisher offline or `update_oracle_from_pyth`
failing.

**Immediate**:
1. Switch to fallback oracle (Switchboard / authority-signed) if
   configured.
2. Authority calls `set_market_status(PostOnly)` — no new positions,
   but existing closes and liquidations remain (with the staleness
   gate blocking until oracle returns).

**Investigate**: Pyth status page, validator slot lag, RPC connectivity.

**Recovery**: once oracle resumes, `set_market_status(Active)`.

### 5. Keeper bot offline / no liquidations firing

**Detection**: unhealthy positions accumulating per `assess_margin`
queries. No `LiquidationInjectedEvent` despite undercollateralized
positions on-chain.

**Immediate**:
1. Page on-call. Re-launch keeper instance.
2. Manually run `liquidate_position_v2` against the worst positions
   from any wallet (permissionless).

**Investigate**: keeper RPC failover, slot lag, transaction landing
rate (Solana congestion).

### 6. Mark price drifts from oracle (mark-EMA stuck)

**Detection**: `MarkPriceDriftEvent` repeating. `mark_price_ticks`
diverged from `oracle_price_ticks` by > `mark_max_change_bps`.

**Immediate**: anyone calls `settle_mark` (permissionless) to hard-
reset mark = oracle.

**Investigate**: was the matcher producing extreme fills that overrode
the EMA clamp? Check `FillBatchEvent` history.

---

## P2 scenarios

### 7. Funding rate spiking / oscillating

**Detection**: `FundingSettledEvent` rates > 1% per hour or rapid
sign-flips slot-to-slot.

**Immediate**: nothing if funding-velocity smoothing is
wired in. Pre-wire-in, authority can call `update_market_params` to
tighten `funding_per_period_max_bps`.

**Investigate**: OI imbalance. Identify large traders driving the
skew; expected behavior of the funding mechanism.

### 8. FLP NAV/share dropping unexpectedly

**Detection**: `FlpCapitalUpdatedEvent.new_total` declining despite no
withdrawals.

**Immediate**: trace through `apply_flp_fill` events. FLP is the
counterparty to large profitable trader closes — declining NAV is
expected when the pool is on the losing side of flows. Confirm via
`FillAppliedEvent` aggregation.

**Investigate**: VPIN toxicity score, OI imbalance, whether
`flp_quoter` is producing stale or off-market quotes.

### 9. ER commit-buffer staleness

**Detection**: ER state lagging mainnet by > commit interval.

**Immediate**: authority calls `undelegate_market_book` +
`undelegate_market` + `undelegate_commit_buffer`. Operations continue
on mainnet (slower but functional). Re-delegate after ER recovers.

---

## Post-incident

For every P0 / P1:

1. **Public incident report** within 24 hours. Template:
   - Summary
   - Timeline
   - Detection signal
   - Root cause
   - Immediate action taken
   - User impact (positions, balances, refunds)
   - Long-term remediation

2. **Add monitoring** for the missed signal.

3. **Update this playbook** if a new failure mode was discovered.

4. **Patch + deploy** any code fix via staged path
   (devnet → testnet → mainnet small market → mainnet full).

5. **External audit** of the patched code if the bug was in audited
   surface.

## Kill switches reference

| Kill | Effect | Authority required |
|---|---|---|
| `set_market_status(Paused)` | No new orders, no fills | per-market authority |
| `set_market_status(PostOnly)` | Limit orders only, no aggressors | per-market authority |
| `set_market_status(Closed)` | Terminal — only close-existing | per-market authority |
| `set_market_status(Active)` | Restore | per-market authority |
| Envelope tightening | Lower `max_price_move_bps_per_slot` | per-market authority |
| Burn authority | Permanent decentralization (irreversible) | per-market authority |
| FLP deposit pause | (future) — block new FLP deposits | per-market authority |

Once `burn_market_authority` has been called, NONE of the above admin
kill switches are available. Operators should burn only after a market
has stabilized for an extended operating period.

## Communication channels

- **Internal pager**: PagerDuty (or equivalent)
- **Status page**: `status.flashbook.example` (placeholder)
- **Public**: Twitter / Discord (links TBD)
- **Bilateral with audit partners**: Sherlock contact, Trail of Bits
  retainer

Update this section with concrete handles when an external comms
infrastructure is in place.
