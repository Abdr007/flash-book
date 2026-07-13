# Governance

On-chain governance of a Clober market is built around one principle:
**asymmetry**. Fail-safe actions (pause, tighten, veto) are fast and
low-privilege; fail-dangerous actions (unpause, loosen, transfer control)
are slow, explicit, and bound to exactly what was pre-announced.

All governance state lives in **separate, small PDAs** — never in new
`MarketAccount` fields. `MarketAccount` (~1152 bytes) is deserialized
unboxed in many instruction frames that sit near the 4 KB BPF stack limit,
so growing it is a hard migration; a dedicated PDA adds bytes only to the
one admin context that reads it and never touches the hot path.

## Emergency guardian (restrict-only)

`set_guardian` (authority-only) sets or clears a `MarketGuardianAccount`
PDA (`["market_guardian", market]`). The guardian's powers are strictly
asymmetric:

- `set_market_status`: the guardian may move a live market only to a
  **more restricted** status (PostOnly → Paused → Closed, monotone on the
  status ladder). Any loosening — unpausing, reactivating — is
  authority-only. A compromised guardian can halt trading; it can never
  reopen a market or move funds.
- `guardian_veto_param_update`: the guardian may cancel a pending
  timelocked params update during its delay window — the brake against a
  compromised authority proposing a dangerous loosening.

## Two-step authority transfer

`transfer_market_authority` (immediate, zero-key-rejected) is retained,
but the production path is two-step via
`MarketPendingAuthorityAccount` (`["pending_authority", market]`):

1. `propose_authority_transfer` (current authority) stores the pending key.
2. `accept_authority_transfer` — signed **by the new key** — commits
   `market.authority` and closes the pending account.
3. `cancel_authority_transfer` (current authority) revokes a pending
   proposal.

Because the new key must sign to accept, control can never strand at a
mistyped or dead key.

## Timelocked parameter updates

All economic parameter changes (fees, margins, funding, oracle band, LP
coefficients, …) go through the timelocked path via `PendingParamUpdateAccount`
(`["pending_params", market]`). **K-3:** the immediate `update_market_params`
instruction no longer changes economic params — it is restricted to a single
safety operation, enabling a *disabled* (legacy, pre-bound-era)
oracle-staleness gate (`oracle_staleness_max_seconds == 0` → a sane
`[MIN_HEAL_STALENESS_SECONDS, MAX_HEAL_STALENESS_SECONDS]` value), with every
other field required byte-identical to the live params. So it cannot change
fees/margins/funding without notice; the timelock is the only path for those:

1. `propose_param_update` validates the new params (`validate_market_params`)
   and stores
   `keccak(params)` plus `eta = now + PARAM_UPDATE_TIMELOCK_SECONDS`
   (48 hours). Nothing is applied. `ParamUpdateProposedEvent` carries the
   eta so LPs and traders can see the pre-announced change and react.
2. `execute_param_update` applies only when `now >= eta` **and** the
   supplied params hash to the stored keccak — the executed change is
   byte-identical to the announced one. It re-validates and closes the
   pending account.
3. `cancel_param_update` (authority) or `guardian_veto_param_update`
   (guardian) revoke a pending update.

## One-way oracle-source lock

`lock_oracle_source` (authority-only, irreversible — there is no unlock
instruction) permanently disables the direct-authority oracle paths
(`update_oracle`, `update_oracle_quorum`) on a market, leaving only the
trustless Pyth and Lazer ingestion paths. The lock flag lives inside the
envelope-config account that the direct paths already **require**, so it
cannot be bypassed by omitting an optional account, and re-configuring the
envelope never touches it.

## Authority burn and sequencer rotation

- `burn_market_authority` permanently relinquishes market authority — the
  end state for a market that should live under immutable parameters.
- `set_market_sequencer` rotates the fill-settlement signer; the
  commitment ring keeps settlement authenticity invariant across
  rotations.
- `set_sequencer_committee` creates or rotates the BFT validator-set
  primitive (see `docs/DECENTRALIZED_SEQUENCER.md`); rotation clears
  equivocation-jail state and bumps the committee epoch.

## Scope and residuals

- The **program upgrade authority** (BPF loader) is outside instruction
  gating entirely: whoever holds it can replace the program. It belongs in
  an M-of-N multisig.
- Timelocks protect against a compromised single key; they do not protect
  against M-of-N collusion. The multisig posture raises the bar; it is not
  trustlessness.
- Sequencer ordering/liveness trust is a separate boundary — see
  `ER_TRUST_BOUNDARY.md`.

## Operational status

The code supports multisig authorities everywhere an authority signs
(a multisig PDA is just a key). Moving the live upgrade authority,
`market.authority`, and `insurance_fund.authority` onto an M-of-N multisig
is an operational step, not a code change — the runbook is in
`docs/OPERATIONS.md`.
