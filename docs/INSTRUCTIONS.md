# Instruction reference

The program exposes 146 Anchor instructions. `idl/flash_book.json` is the
source of truth for accounts and argument layouts; handler doc comments in
`programs/flash-book/src/lib.rs` are the authoritative behavior contracts.
This page is the grouped index.

Access legend — **authority**: gated on `market.authority` (or the stated
account's authority) · **guardian**: market's emergency guardian ·
**sequencer**: the market's settlement signer · **permissionless**: any
signer · **trader**: the owning trader (or their delegate/session where
noted).

## Market lifecycle

| Instruction | Access | Purpose |
|---|---|---|
| `initialize_market` | protocol authority | Create a market PDA with validated params. |
| `init_market_book` | authority | Allocate the raw order-book slab PDA. |
| `expand_market_book` | authority | Grow the slab (drained-invariant preserved). |
| `migrate_market_to_v3` | authority | Realloc a market account to the v3 size (bounded by the 10 MB account cap). |
| `reseat_order_seq_counter` | authority | Reseat the order-sequence counter on an empty book (emits an event). |
| `seed_residual` | authority | Seed the haircut residual bucket. |
| `verify_market_invariants` | permissionless | Probe internal consistency; auto-pauses on OI drift or a stale mark. |

## Orders (book)

| Instruction | Access | Purpose |
|---|---|---|
| `place_limit_order_v2` | trader | Rest a limit order (post-only/IOC/FOK/reduce-only/STP flags, GTT expiry, oracle-band placement gate, intake initial-margin gate). |
| `place_taker_order_v2` | sequencer-driven matching | Walk the book, produce fills into the ring/outbox (runs on the ER). |
| `modify_order_v2` | trader | Cancel-and-replace preserving ownership and sub-account. |
| `cancel_order_v2` | trader | Remove a resting order. |
| `cancel_all_v2` | trader | Flatten all of the trader's resting orders in a market. |
| `reap_expired_orders` | permissionless | Reclaim expired resting orders (bounded per call). |
| `place_basket_order_v2` / `place_basket_order_n_v2` | trader | Multi-leg orders under a joint cross-market margin gate. |

## Trigger / TWAP / iceberg / bracket

| Instruction | Access | Purpose |
|---|---|---|
| `place_trigger_order_v3` | trader | Stop / take-profit trigger PDA (slippage cap, reduce-only, OCO-capable). |
| `execute_trigger_order_v3` | permissionless | Fire a triggered order into the book at a fresh, gated oracle price. |
| `cancel_trigger_order_v3` | trader | Close a trigger PDA. |
| `place_twap_order_v3` / `execute_twap_slice_v3` / `cancel_twap_order_v3` | trader / permissionless / trader | Sliced execution over time with slippage caps. |
| `place_iceberg_order_v3` / `replenish_iceberg_v3` / `cancel_iceberg_v3` | trader / permissionless / trader | Hidden reservoir + visible-chunk replenishment. |
| `place_bracket_order_v3` | trader | Atomic parent limit + two OCO-linked trigger legs. |

## Settlement & oracle

| Instruction | Access | Purpose |
|---|---|---|
| `apply_fill` | sequencer (permissionless keeper on armed markets) | Settle one matched fill against ring authenticity; moves positions, collateral, fees, funding, OI. |
| `apply_flp_fill` | sequencer (same armed-market rule) | Settle a pool-maker fill under ring + oracle-band bounds. |
| `settle_funding` | permissionless | Settle a position's accrued funding via the side-accrual indices. |
| `settle_mark` | authority | Hard mark write under the envelope gate. |
| `update_oracle` / `update_oracle_quorum` | authority (reverts once source-locked) | Direct oracle writes under staleness + envelope gates. |
| `update_oracle_from_pyth` | permissionless | Ingest a fully-verified Pyth `PriceUpdateV2` under the envelope gate. |
| `update_oracle_from_lazer` | permissionless | Ingest an Ed25519-verified Lazer payload with a strictly-increasing replay nonce. |
| `init_market_oracle_config` / `init_lazer_oracle_config` | authority | Bind and bound the oracle sources. |
| `gate_envelope_price_move` | permissionless probe | Prove a price move admissible under the envelope. |
| `init_fill_commitment` / `grow_fill_commitment` | authority | Allocate / grow the settlement ring (arming is sticky; grow requires a drained ring). |
| `upgrade_fill_commitment_v1` | authority | One-way upgrade of a drained ring to the v1 layout with reduce-in-flight tracking. |
| `reconcile_unsettled_fill_volume` | permissionless | Reset the matched-but-unsettled OI reserve when the ring is drained. |
| `init_fill_outbox` / `grow_fill_outbox` | authority | Allocate / grow the fill outbox (cap mirrors the ring; grow requires drained). |

## ER / delegation

| Instruction | Access | Purpose |
|---|---|---|
| `delegate_market_book` / `delegate_fill_commitment` / `delegate_fill_outbox` / `delegate_market` | authority | Delegate the matching-domain accounts to the ER. |
| `commit_market_book` / `commit_fill_commitment` / `commit_fill_outbox` | permissionless (ER-side) | Snapshot delegated state back to L1. |
| `commit_and_undelegate_*` | sequencer-gated | Snapshot + queue undelegation. |
| `process_undelegation` | delegation-program callback | Finalize undelegation; buffer bound to the canonical DLP PDA. |
| `force_undelegate_market_book` | permissionless | Intended escape hatch after ER stall / censorship timeouts (Kani-proven gate). **Not executable against the deployed delegation program** — undelegation is validator-driven, so the gate opens but the handler returns `OwnerForceUndelegateUnavailable`; the working exit is sequencer-gated `commit_and_undelegate_market_book` (see `ER_TRUST_BOUNDARY.md` §1.1). |
| `stamp_book_liveness_baseline` | authority | Stamp a liveness baseline for books delegated before baselines existed. |
| `er_heartbeat` | sequencer | ER liveness heartbeat (keeps the fast escape shut on quiet-but-live markets). |
| `init_er_margin_attestation` / `attest_er_reserved_margin` | authority / sequencer | Attest ER-reserved margin for cross-domain withdrawal floors. |

## Privacy & sessions

| Instruction | Access | Purpose |
|---|---|---|
| `init_book_permission` / `set_book_privacy` / `close_book_permission` | authority | Manage the TEE private-ER read allow-list (see `docs/PRIVACY.md`). |
| `create_session_token` / `revoke_session_token` | trader | Scoped, expiring session keys (optionally market-scoped). |
| `place_limit_order_v2_session` / `cancel_order_v2_session` / `deposit_collateral_session` | session key | Session-signed variants. |

## FLP (pool)

| Instruction | Access | Purpose |
|---|---|---|
| `initialize_flp_exposure` | protocol authority | Create the pool exposure singleton. |
| `deposit_flp_capital` / `withdraw_flp_capital` | LP | NAV-share capital in/out (minimum-hold gate on exit). |
| `flp_post_maker_order` | authority | Post a pool maker order. |
| `flp_refresh_quotes` | permissionless (rate-limited) | Regenerate the deterministic pool quote ladder. |
| `init_flp_per_market_v3` | protocol authority | Per-market pool exposure account. |
| `flp_deposit_v3` / `flp_withdraw_v3` | LP | Per-market NAV-based capital (withdraw blocked while the pool has open positions against it). |
| `record_flp_fill_v3` | sequencer | Record per-market pool exposure deltas. |

## Vaults (v3)

| Instruction | Access | Purpose |
|---|---|---|
| `create_vault_v3` / `vault_open_trader_state_v3` | strategist | Create a vault and its trading identity. |
| `vault_deposit_v3` / `vault_withdraw_v3` | depositor | NAV-share deposits/withdrawals (withdraw blocked with open positions). |
| `vault_place_order_v3` / `vault_cancel_order_v3` | strategist | Trade the vault's book presence. |
| `settle_vault_perf_fee_v3` | strategist | High-water-mark performance fee in shares. |

## Trader & collateral

| Instruction | Access | Purpose |
|---|---|---|
| `open_trader_state` / `open_trader_sub_account` | trader | Main and sub-account TraderStates. |
| `deposit_collateral` / `withdraw_collateral` / `partial_withdraw_collateral` | trader | Collateral in/out under the initial-margin withdraw gate and ER-reserved floors. |
| `withdraw_collateral_xdomain` / `partial_withdraw_collateral_xdomain` | trader | Withdrawals honoring ER-attested reserved margin. |
| `transfer_main_to_sub` / `transfer_sub_to_main` | trader | Sub-account collateral moves (margin-gated outbound). |
| `sweep_collateral` | trader | Cross-account consolidation under a joint stress gate. |
| `init_trader_ata` / `close_trader_ata` | trader | Program-validated associated token accounts. |
| `migrate_position_to_trader_state_key` | trader | One-time move of a legacy position PDA to the trader-state-keyed derivation. |
| `set_position_leverage` / `set_position_isolated` / `set_position_cross` | trader | Leverage cap and margin-mode switches (coverage-gated). |
| `set_trader_referrer` / `set_trader_builder` / `set_trader_delegate` | trader | One-time referrer, capped builder code, trading delegate. |
| `set_trader_fee_tier` | authority | Assign a fee tier (discount capped at 100%). |

## Liquidation & ADL

| Instruction | Access | Purpose |
|---|---|---|
| `liquidate_position_v2` | permissionless | Close an unhealthy position at the worse-of(mark, oracle) health price; reward bounded by residual equity; no self-liquidation; blocked while paused. |
| `liquidate_portfolio_v2` | permissionless | Portfolio-level liquidation via synthetic close orders. |
| `auto_deleverage` | permissionless | Force-close the best counter-position at the bankruptcy price, only against true bankruptcy, value-conserving; blocked while paused. |
| `place_jit_liquidation_offer` / `cancel_jit_liquidation_offer` | maker | Bid to absorb liquidations at better-than-synthetic prices. |

## Insurance, haircut & solvency

| Instruction | Access | Purpose |
|---|---|---|
| `initialize_insurance_fund` | protocol authority | Create the fund singleton. |
| `withdraw_insurance_fund` | fund authority | Withdraw above the pause threshold only. |
| `set_insurance_pause_threshold` | fund authority | Tune the ADL/pause trigger floor. |
| `verify_protocol_solvency` / `verify_collateral_solvency` | permissionless | On-chain solvency probes (one-sided insolvency detectors; machine-proven sound). |
| `verify_haircut_invariants` | permissionless | Haircut internal-consistency report. |
| `initialize_haircut_state` / `init_position_haircut_state` | authority / trader | Enable the haircut engine (sticky) and per-position state. |
| `mature_position` / `convert_position` / `release_gain_to_haircut` / `flush_haircut_dust` | permissionless / trader | Reserve → matured → converted profit pipeline; dust to insurance. |
| `initialize_side_accrual` | authority | Per-side A/K/F/B accrual indices account. |
| `set_envelope_config` / `verify_envelope_config` | authority / permissionless | Per-market envelope bounds (solvency-checked at write). |

## Governance & admin

| Instruction | Access | Purpose |
|---|---|---|
| `set_market_status` | authority or guardian | Status changes; the guardian may only restrict (never unpause). |
| `set_oi_insurance_multiple_bps` | authority | G-3: set/clear the OI-vs-insurance circuit-breaker multiple (0 = disabled). When gross OI notional exceeds `insurance · bps / 10_000`, settlement auto-pauses the market. |
| `set_guardian` | authority | Set/clear the emergency guardian PDA. |
| `update_market_params` | authority | RESTRICTED (K-3): immediate path may ONLY enable a disabled oracle-staleness gate; all economic changes go through the timelock. |
| `propose_param_update` / `execute_param_update` / `cancel_param_update` | authority | 48 h timelocked params path bound to a keccak params-hash. |
| `guardian_veto_param_update` | guardian | Veto a pending params update during its delay. |
| `transfer_market_authority` | authority | Immediate transfer (rejects the zero key). |
| `propose_authority_transfer` / `accept_authority_transfer` / `cancel_authority_transfer` | authority / new key / authority | Two-step transfer; the new key must sign to accept. |
| `lock_oracle_source` | authority | One-way: permanently disables direct-authority oracle writes. |
| `burn_market_authority` | authority | Irreversibly relinquish authority. |
| `set_market_sequencer` | authority | Rotate the fill-settlement signer. |
| `init_fee_tiers` / `update_fee_tiers` | authority | Volume-tier fee table (validated, capped). |

## Sequencer committee

| Instruction | Access | Purpose |
|---|---|---|
| `set_sequencer_committee` | authority | Create/rotate a BFT validator set (quorum-validated; clears jail state). |
| `commit_batch` | permissionless | Record a committee-attested state transition (quorum of Ed25519 attestations, replay-guarded, root-chained). Settlement is not gated on this. |
| `slash_equivocation` | permissionless | Jail a validator on proof of two conflicting signed batches at one height. |

## Views

| Instruction | Access | Purpose |
|---|---|---|
| `view_book_depth_v2` / `view_quote_ladder` / `view_predicted_funding` / `view_portfolio_risk` / `view_trader_effective_tier` | permissionless (simulate) | Event-emitting read probes for UIs via transaction simulation. |
