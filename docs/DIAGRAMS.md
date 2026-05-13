# Architecture Diagrams

All diagrams are [Mermaid](https://mermaid.js.org/) — GitHub renders
them natively when you open this file on github.com. Source-controlled
as text so they update with the code.

## 1. System overview

```mermaid
flowchart TB
    subgraph clients["Client side"]
        bot["Reference MM bot<br/>(bot/)"]
        ui["Front-end<br/>(separate repo)"]
        keeper["Keeper suite<br/>(bot/keepers.ts)"]
    end

    subgraph sdk["SDK layer"]
        client["FlashBookClient<br/>(sdk-ts/src/client.ts)"]
        pdas["PDA derivation<br/>(sdk-ts/src/pdas.ts)"]
        events["Event decoders<br/>(sdk-ts/src/events.ts)"]
    end

    subgraph chain["Solana mainnet/devnet"]
        program["Flash Book program<br/>Di8ZzxmMb5Ho2xWHbvcAxKPjcaVXTCM7U5xe5Gm7uLVF"]
        pyth["Pyth oracle"]
        spl["SPL token program"]
    end

    subgraph storage["On-chain state (PDAs)"]
        market["MarketAccount<br/>per market"]
        book["market_book<br/>hypertree"]
        ts["TraderStateAccount<br/>main + 255 subs/wallet"]
        pos["PositionAccount<br/>per (market, trader_state)"]
        flp["FlpExposureAccount<br/>global LP pool"]
        ifund["InsuranceFundAccount<br/>global"]
    end

    bot --> client
    ui --> client
    keeper --> client
    client --> program
    pdas --> client
    events --> client
    program --> market
    program --> book
    program --> ts
    program --> pos
    program --> flp
    program --> ifund
    program --> pyth
    program --> spl
```

## 2. Account / PDA ownership tree

```mermaid
flowchart LR
    wallet["wallet<br/>(Signer)"]
    main_ts["TraderStateAccount<br/>main<br/>[STATE_SEED, wallet]"]
    sub_ts["TraderStateAccount<br/>sub_index=1..255<br/>[STATE_SEED, wallet, sub_index]"]
    main_pos["PositionAccount<br/>main, per market<br/>[POS_SEED, market, main_ts]"]
    sub_pos["PositionAccount<br/>sub, per market<br/>[POS_SEED, market, sub_ts]"]
    main_lp["LpPositionAccount<br/>[LP_SEED, wallet]"]
    builder["BuilderAccount<br/>(referral / fee share)"]
    delegate["delegate slot<br/>(on main_ts)"]

    wallet --signs--> main_ts
    wallet --signs--> sub_ts
    wallet --signs--> main_lp
    main_ts -. position-key seeds .-> main_pos
    sub_ts -. position-key seeds .-> sub_pos
    main_ts --> builder
    main_ts --> delegate
```

Each TraderStateAccount has its own positions per market. Phase 2c made
this strict — Position PDAs key on the trader_state pubkey, so main +
sub-accounts on the same wallet have distinct positions per market. This
is what makes risk-isolation between main and sub mechanically guaranteed.

## 3. Order placement → fill → settlement flow

```mermaid
sequenceDiagram
    autonumber
    participant T as Trader (signer)
    participant Prog as Flash Book program
    participant Book as market_book (hypertree)
    participant Seq as Off-chain sequencer
    participant Apply as apply_fill ix

    T->>Prog: place_limit_order_v2(side, size, limit, sub_index)
    Prog->>Book: insert RestingOrderV2<br/>(carries trader + sub_index)
    Note over Book: Order rests in hypertree

    T->>Prog: place_taker_order_v2(side, size, limit, sub_index)
    Prog->>Book: walk opposite side<br/>best-price-first
    Book-->>Prog: matched orders + maker_sub_index per fill
    Prog->>Prog: emit FillBatchEvent<br/>(carries taker_sub_index)<br/>(each FillEntry carries maker_sub_index)

    Seq->>Seq: read FillBatchEvent
    Seq->>Apply: apply_fill(size, price, taker_side,<br/>taker_sub_index, maker_sub_index)
    Apply->>Apply: verify_trader_state_pda<br/>for both sides (Phase 2i)
    Apply->>Apply: fee routing per bucket<br/>(Phase 2b)
    Apply->>Apply: apply_fill_to_position<br/>(updates size, entry, realized_pnl)
    Apply->>Apply: route realized PnL delta<br/>to collateral bucket (Phase 2g)
    Apply->>Apply: update OI counters
    Apply-->>Seq: FillAppliedEvent
```

## 4. Cross-margin vs isolated-margin bucket independence (Phase 2)

```mermaid
flowchart TB
    subgraph trader_T["Trader T"]
        C_T["C_T<br/>cross pool<br/>(trader_state.collateral_quote_lots)"]
        C_a["c_a<br/>isolated bucket<br/>position_a.collateral_quote_lots > 0"]
        C_b["C_b<br/>cross-margined<br/>position_b.collateral_quote_lots = 0"]
        C_c["c_c<br/>isolated bucket<br/>position_c.collateral_quote_lots > 0"]
    end

    subgraph health_check["assess_margin_unified"]
        cross_check["Cross bucket<br/>{P_b}<br/>vs C_T"]
        iso_a_check["Isolated bucket<br/>{P_a}<br/>vs c_a alone"]
        iso_c_check["Isolated bucket<br/>{P_c}<br/>vs c_c alone"]
    end

    C_T --> cross_check
    C_b -. evaluated only in cross set .-> cross_check
    C_a --> iso_a_check
    C_c --> iso_c_check

    cross_check -- and --> healthy{Healthy iff<br/>ALL buckets pass}
    iso_a_check -- and --> healthy
    iso_c_check -- and --> healthy
```

Invariant I-3: an isolated bucket failure cannot bleed into C_T.
Invariant I-4: a cross-set failure cannot debit any c_m.

## 5. Liquidation pipeline

```mermaid
flowchart TB
    Start["Position becomes unhealthy"] --> Trigger["Any keeper calls<br/>liquidate_position_v2"]
    Trigger --> Gate{"Health gate:<br/>worse-of(mark, oracle)<br/>+ stale-oracle check"}
    Gate -- healthy --> Reject["Reject:<br/>NotLiquidatable"]
    Gate -- unhealthy --> JIT{"Walk JIT auction:<br/>any maker offer<br/>better than synthetic?"}

    JIT -- yes --> JITFill["Use JIT price<br/>for synthetic close"]
    JIT -- no --> SynPrice["Use synthetic price<br/>oracle ± liq_penalty_bps"]

    JITFill --> Inject["Inject synthetic close<br/>into hypertree<br/>(order_type = 3 Liquidation,<br/>sub_index = liquidatee's)"]
    SynPrice --> Inject

    Inject --> Reward{"Dutch-auction reward<br/>scales 0% → 100%<br/>over auction_duration_slots"}
    Reward --> RoutingReward{"Position isolated?"}
    RoutingReward -- yes --> RewardIso["Debit<br/>position.collateral_quote_lots<br/>(saturating)"]
    RoutingReward -- no --> RewardCross["Debit<br/>trader_state.collateral_quote_lots"]

    RewardIso --> CallerCredit["Credit caller_trader_state"]
    RewardCross --> CallerCredit

    CallerCredit --> Tape["Synthetic close rests<br/>or fills immediately"]
    Tape --> InsCheck{"Insurance fund<br/>below pause threshold?"}
    InsCheck -- no --> Done["Done"]
    InsCheck -- yes --> ADL["Escalate to auto_deleverage<br/>(Phase 2h per-bucket routing)"]
    ADL --> Done
```

## 6. Phase 2 series — what shipped in v0.2.0

```mermaid
gantt
    title Phase 2 series — isolated margin + sub-account trading
    dateFormat YYYY-MM-DD
    section Foundation
    Phase 1 scaffolding         :p1, 2026-05-12, 1d
    section Risk engine
    Phase 2 split risk + reward routing  :p2, after p1, 1d
    Phase 2b apply_fill fee routing :p2b, after p2, 1d
    section Sub-accounts
    Phase 2c Position PDA migration :p2c, after p2b, 1d
    Phase 2d trader_state seed relax :p2d, after p2c, 1d
    Phase 2e RestingOrderV2.sub_index :p2e, after p2d, 1d
    Phase 2f trigger/TWAP/iceberg/bracket/JIT :p2f, after p2e, 1d
    section Gap closures
    Phase 2g realized PnL materialisation :p2g, after p2f, 1d
    Phase 2h ADL settlement routing :p2h, after p2g, 1d
    Phase 2i ApplyFill PDA verification :p2i, after p2h, 1d
    Phase 2j ApplyFill integration tests :p2j, after p2i, 1d
```

Released as `v0.2.0` on `2026-05-14`.

## 7. Phase 3 / future work (NOT in v0.2.0)

```mermaid
flowchart LR
    v02["v0.2.0<br/>Phase 2 complete"] --> v05_choice{"Phase 3<br/>next slice?"}
    v05_choice --> hlp["v0.5.0 (planned)<br/>HLP-equivalent<br/>backstop vault<br/>(docs/HLP_BACKSTOP_VAULT.md)"]

    classDef rejected fill:#fee,stroke:#900,color:#900
    fba["FBA on-chain<br/>(rejected by design)"]:::rejected
    cr["Commit-reveal on-chain<br/>(rejected by design)"]:::rejected

    v0_2["v0.2.0 docs"] -.- fba
    v0_2 -.- cr
```

Continuous CLOB on-chain is the deliberate architectural pick.
FBA / commit-reveal stay research-only in `src/`. See
`docs/COMPARISON.md` for the rationale.

## 8. Layout — where each thing lives in the repo

```mermaid
graph LR
    subgraph repo["flash-book/"]
        progs["programs/flash-book/<br/>on-chain Anchor program (Rust)"]
        sdk["sdk-ts/<br/>TypeScript client"]
        bot["bot/<br/>reference MM bot + keepers"]
        src["src/<br/>TS research simulator (FBA, CR)"]
        tests["tests/<br/>TS test suite"]
        docs["docs/<br/>specs + scope docs"]
    end

    subgraph chain["What ships on-chain"]
        prog["Anchor program<br/>(continuous CLOB matcher,<br/>isolated margin Phase 2,<br/>liquidation engine)"]
    end

    progs --> prog
    sdk -. interacts with .-> prog
    bot -. signs txs to .-> prog
    src -. modelling only,<br/>NOT shipped .- prog
```
