# Flash Book — System Architecture

End-to-end architecture diagrams covering every subsystem added across
Waves 18–65.

---

## 1. System landscape

```mermaid
flowchart TB
    subgraph clients[Clients]
        ui[UI / Web app]
        bot[MM bot]
        keeper[Keeper bot]
        wallet[Wallet]
    end

    subgraph sdk_layer[SDK layer]
        ts_sdk["@flash-book/sdk<br/>(TypeScript)"]
        parity["Parity math (src/)"]
    end

    subgraph onchain[Solana on-chain]
        prog["flash_book program<br/>(Anchor)"]
        market_book["MarketBook PDA<br/>(hypertree)"]
        market["MarketAccount"]
        positions["Position PDAs"]
        haircut["MarketHaircutState"]
        envelope["MarketEnvelopeConfig"]
        side_accrual["MarketSideAccrual"]
        vault["SPL Quote Vault"]
        insurance["InsuranceFund"]
        flp["FlpExposure"]
    end

    subgraph oracles[Oracles]
        pyth[Pyth Receiver]
        switchboard[Switchboard]
    end

    subgraph er[MagicBlock ER]
        er_validator["ER Validator<br/>(sub-ms tick)"]
    end

    ui --> ts_sdk
    bot --> ts_sdk
    keeper --> ts_sdk
    wallet --> ts_sdk
    ts_sdk --> parity
    ts_sdk --> prog
    prog --> market_book
    prog --> market
    prog --> positions
    prog --> haircut
    prog --> envelope
    prog --> side_accrual
    prog --> vault
    prog --> insurance
    prog --> flp
    pyth --> prog
    switchboard -.future.-> prog
    prog -.delegate.-> er_validator
    er_validator -.commit.-> prog
```

---

## 2. Account ownership tree

```text
                       ┌─────────────────────────┐
                       │ Market authority signer │
                       └──────────┬──────────────┘
                                  │ inits
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│                   MarketAccount (per market)                    │
│  seeds: [b"market", base_mint, quote_mint]                      │
│  fields: authority, params, oracle_*, oi_*, mark_*, status      │
└──┬──────────────────────────────────────────────────────────────┘
   │
   ├──► MarketBook (hypertree)  [b"market_book", market]
   ├──► MarketHaircutState      [b"haircut", market]
   ├──► MarketSideAccrual       [b"side_accrual", market]
   ├──► MarketEnvelopeConfig    [b"envelope", market]
   ├──► MarketLeverageTiers     [b"leverage_tiers", market]
   ├──► MarketOracleConfig      [b"oracle_config", market]
   ├──► CommitBuffer            [b"commit_buffer", market]
   │
   └──► Position PDAs (per trader, per market)
                seeds: [b"position", market, trader_state]
                ├──► PositionHaircutState  [b"position_haircut", market, position]
                └──► Sibling triggers / TWAPs / brackets / iceberg orders

                       ┌──────────────────────────┐
                       │ InsuranceFundAccount     │  [b"insurance_fund"]
                       │ (singleton, owns vault)  │
                       └───────────┬──────────────┘
                                   │ owns
                                   ▼
                            ┌───────────────┐
                            │ Quote Vault   │  (SPL TokenAccount)
                            └───────────────┘
                                   ▲
                                   │
                       ┌───────────┴──────────────┐
                       │ FlpExposureAccount       │  [b"flp_exposure"]
                       │ (singleton, NAV shares)  │
                       └──────────────────────────┘

                       ┌──────────────────────────┐
                       │ TraderStateAccount       │  [b"trader_state", wallet[, sub]]
                       │ (per wallet × sub_index) │
                       └──────────────────────────┘
```

---

## 3. Fill flow (apply_fill, post-Wave 24d)

```mermaid
sequenceDiagram
    participant Taker
    participant Matcher as place_taker_order_v2
    participant Book as MarketBook (hypertree)
    participant Seq as Sequencer (off-chain)
    participant ApplyFill as apply_fill
    participant TakerPos as Taker Position
    participant MakerPos as Maker Position
    participant Haircut as Haircut State (optional)
    participant Trader as TraderState collateral

    Taker->>Matcher: place_taker_order_v2(side, size, limit)
    Matcher->>Book: walk opposite-side book
    Book-->>Matcher: candidate matches
    Matcher->>Matcher: STP check (skip / cancel-oldest / cancel-both)
    Matcher->>Book: update maker order sizes
    Matcher->>Seq: emit FillBatchEvent (Vec&lt;FillEntry&gt;)
    
    loop per FillEntry
        Seq->>ApplyFill: apply_fill(taker, maker, size, price)
        ApplyFill->>ApplyFill: snapshot pre-state (sides, sizes, realized)
        ApplyFill->>ApplyFill: charge taker fee + maker rebate (isolated/cross routing)
        ApplyFill->>TakerPos: apply_fill_to_position (weighted avg or close-with-PnL)
        ApplyFill->>MakerPos: apply_fill_to_position
        
        alt Positive PnL delta AND haircut accounts provided
            ApplyFill->>Haircut: apply_release(gain) → reserve += gain
            Note over Haircut: NO collateral mutation
        else Legacy / loss
            ApplyFill->>Trader: apply_realized_pnl_delta (direct)
        end
        
        ApplyFill->>ApplyFill: update OI counters
        ApplyFill->>ApplyFill: blend fill price into mark EMA (clamped)
        ApplyFill-->>Seq: FillAppliedEvent
    end
```

---

## 4. H-haircut lifecycle (Wave 24)

```text
Fill realizes positive PnL
        │
        ▼  apply_release(gain, now_slot)
┌──────────────────────────────────────────┐
│  Position.released_reserve += gain       │
│  attached_at_slot = first-add slot       │
│  original_reserve_at_attach += gain      │
└──────────────────────────────────────────┘
        │
        │  warmup [h_min, h_max] slots
        │  (linear ramp; old reserves keep clock)
        ▼  mature_position()  (permissionless keeper crank)
┌──────────────────────────────────────────┐
│  target_cumulative =                     │
│    matured_fraction(original, attached,  │
│      now, h_min, h_max)                  │
│  delta = target - already_drained        │
│  reserve   -= delta                      │
│  matured   += delta                      │
│  market.matured_pos_total += delta       │
└──────────────────────────────────────────┘
        │
        ▼  convert_position()  (permissionless)
┌──────────────────────────────────────────┐
│  h = min(Residual, MaturedTotal) / Total │
│  credit = floor(matured × h)             │
│  dust   = matured - credit               │
│  collateral += credit  (iso or cross)    │
│  market.residual -= credit               │
│  market.dust_accrued += dust             │
│  market.matured_pos_total -= matured     │
└──────────────────────────────────────────┘
        │
        ▼  flush_haircut_dust()  (permissionless)
┌──────────────────────────────────────────┐
│  insurance.balance += dust_accrued       │
│  market.dust_accrued = 0                 │
└──────────────────────────────────────────┘
```

---

## 5. Liquidation pipeline

```mermaid
flowchart TB
    start[Keeper detects underwater position] --> cooldown{Cooldown<br/>elapsed?}
    cooldown -- No --> reject1[RateLimited]
    cooldown -- Yes --> stale{Oracle<br/>fresh?}
    stale -- No --> reject2[OracleTooStale]
    stale -- Yes --> health[Compute dual-source<br/>health_price = min/max-of mark,oracle]
    health --> assess[assess_margin under stress lattice]
    assess --> healthy{Healthy?}
    healthy -- Yes --> reject3[NotLiquidatable]
    healthy -- No --> jit{JIT offer<br/>present?}
    jit -- Yes --> jit_price["Use JIT price<br/>better than oracle ± liq_penalty"]
    jit -- No --> synth[Synthesize close at oracle ± liq_penalty]
    jit_price --> close[Inject close order]
    synth --> close
    close --> dutch[Apply Dutch-auction liquidator reward<br/>0% → 100% over auction_duration]
    dutch --> isolated{Position<br/>isolated?}
    isolated -- Yes --> iso_pay[Reward from position.collateral_quote_lots]
    isolated -- No --> cross_pay[Reward from trader_state.collateral_quote_lots]
    iso_pay --> insolvent{Position<br/>insolvent?}
    cross_pay --> insolvent
    insolvent -- Yes --> insurance[Drain from insurance fund]
    insurance --> adl{Insurance<br/>below pause<br/>threshold?}
    adl -- Yes --> auto_dlv[auto_deleverage: ADL counter-position]
    adl -- No --> done
    insolvent -- No --> done[Update position.last_liquidated_at_slot]
```

---

## 6. Oracle update + envelope gate (Wave 26b)

```mermaid
sequenceDiagram
    participant Caller
    participant UpdateOracle as update_oracle*
    participant Market
    participant Envelope as MarketEnvelopeConfig (optional)
    
    Caller->>UpdateOracle: new_price, conf, publish_time
    UpdateOracle->>UpdateOracle: staleness check (publish_time vs now)
    UpdateOracle->>UpdateOracle: confidence check (conf/price ≤ max_bps)
    UpdateOracle->>UpdateOracle: quorum dispersion check (if quorum ix)
    
    alt envelope_config provided
        UpdateOracle->>Envelope: read last_observed_slot/price
        UpdateOracle->>UpdateOracle: gate_price_move(p_last, p_new, dt, cap_bps)
        alt move within cap
            UpdateOracle->>Envelope: update last_observed, bump gate_passes
            UpdateOracle->>Market: write new oracle price
        else move exceeds cap
            UpdateOracle->>Envelope: bump gate_rejects
            UpdateOracle-->>Caller: EnvelopePriceMoveExceedsCap (revert)
        end
    else legacy
        UpdateOracle->>Market: write new oracle price (no gate)
    end
```

---

## 7. A/K/F/B settlement (Wave 25 helpers, wire-in Wave 25c queued)

```text
Per side (long, short):
  A — ADL multiplier (starts at ADL_ONE = 10^15)
  K — Mark-price accumulator (signed)
  F — Funding accumulator (signed)
  B — Bankruptcy residual (signed)

On every settle_funding tick (Wave 25c):
  ┌────────────────────────────────────────┐
  │ advance_indices(side, p_new, fr, now)  │
  │   dt = now - side.slot_last            │
  │   K += dp × A                          │
  │   F += p_last × fr × dt × A            │
  │   side.slot_last = now                 │
  │   side.price_last = p_new              │
  └────────────────────────────────────────┘

On every Position touch:
  ┌────────────────────────────────────────┐
  │ settle_position_pnl(basis, side, snap) │
  │   k_delta = (K - k_snap)               │
  │   f_delta = (F - f_snap)               │
  │   pnl = basis × (k_delta - f_delta)    │
  │         × sign / (a_snap × POS_SCALE)  │
  │ → credit/debit collateral              │
  │                                        │
  │ refresh_position_snapshot(pos, side)   │
  │   pos.a_snap = side.a                  │
  │   pos.k_snap = side.k                  │
  │   pos.f_snap = side.f                  │
  │   pos.b_snap = side.b                  │
  └────────────────────────────────────────┘

ADL via reduce_a_pro_rata(side, num, den):
  All opposing positions auto-shrink by num/den
  on their next touch (via `effective_lots`).

Side state machine (Wave 25a):
  Normal --A<MIN_A_SIDE--> DrainOnly --OI=0--> ResetPending
       ^                                                |
       └──── epoch_advance(side) ───────────────────────┘
```

---

## 8. Permissionless keeper crank schedule

```text
Per market, per N seconds:
  every 30s   verify_haircut_invariants(market)
  every 30s   verify_envelope_config(market)
  every 60s   verify_protocol_solvency()              (singleton)
  every 60s   verify_market_invariants(market)
  every 5min  flush_haircut_dust(market)

Per position:
  on warmup elapse   mature_position(market, position)
  when matured > 0   convert_position(market, position)

Underwater detection:
  always-on    liquidate_position_v2 / liquidate_portfolio_v2

Trigger / TWAP execution:
  on oracle X  execute_trigger_order_v3
  on schedule  execute_twap_slice_v3
  on schedule  replenish_iceberg_v2

Oracle update:
  every slot   update_oracle_from_pyth (per market)
```

See [`docs/KEEPER_RUNBOOK.md`](KEEPER_RUNBOOK.md) for the production
cron + monitoring + alerting playbook.
