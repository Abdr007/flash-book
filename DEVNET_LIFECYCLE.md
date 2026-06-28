# Flash Book — full trading lifecycle, PROVEN on devnet

The original `DEVNET_TXNS.md` landed 47 instruction *types* but never crossed a trade, opened a
position, settled funding, or liquidated anything (taker `match_count=0`, OI `0/0`, `open_positions=0`).
This run closes that gap with **real matched trades, real open interest, real funding, and a real
liquidation** — all signed by `GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD` on devnet, every event
decoded from the on-chain logs.

Program `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` · 23 txns · 379,538 CU total.

## 1. First real fill — matching engine crosses a trade
Market `3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q`. Maker (trader2 `TDQ6BR6S…`) rests an ASK,
taker (signer) buys through it.

| ix | result | CU | tx |
|---|---|---|---|
| place_limit_order_v2 (MAKER ASK 5 @ 100000) | `OrderPlacedV2Event` seq=9 | 13,312 | `2Cfzbms…` |
| place_taker_order_v2 (TAKER BUY 5 @ 100000) | **`FillBatchEvent` filled_lots=5, match_count=1, residual=0** | 15,113 | `4qkesqJ…` |
| **apply_fill** (sequencer settle) | **`FillAppliedEvent` 5 @ 100000 → positions open** | 39,988 | `5sACrRu…` |

Result: **signer LONG 5 @ 100000, trader2 SHORT 5 @ 100000, market OI long=5 short=5** (was 0/0).
Explorer: https://explorer.solana.com/tx/5sACrRuAaqdeLp6wvyTjC2TtxkHRwbDXfMMDhuxg3wyDGyjFUk1ChHRYdrNqU2m1BiuKsVidswkupxgCuR35KuLN?cluster=devnet

## 2. Funding settled on an open position
| ix | result | CU | tx |
|---|---|---|---|
| initialize_haircut_state | `HaircutInitializedEvent` | 17,642 | `M4sDvrX…` |
| settle_funding | **`FundingSettledEvent` owed=0** (balanced OI → 0 premium, correct) | 14,069 | `4cNti4Q…` |

## 3. Real liquidation — risk engine fires
Fresh market `J4k2WarNBUTTX26azgYv28AYocQehEQaKjyQunpTqxKc` (no haircut). Victim (trader3 `EFM2UENa…`)
opens **LONG 100 @ 100000 = 25× leverage on $0.40 collateral**; oracle pushed −5%.

| ix | result | CU | tx |
|---|---|---|---|
| initialize_market (fresh) | `MarketInitializedEvent` | 23,653 | `3RJKsQe…` |
| init_market_book / init_fill_commitment | book + ring | 10,191 / 17,140 | `TTSiMC6…` / `2bPgKDY…` |
| apply_fill (open victim 100-lot long) | `FillAppliedEvent` notional 10,000,000 | 41,279 | `wMXZ5MB…` |
| update_oracle (100000 → 95000) | oracle −5% | 11,299 | `B9RXxny…` |
| **liquidate_position_v2** | **`HealthGateSourceEvent` (worse-of mark/oracle=95000) + `LiquidationInjectedV2Event` worst_scenario_idx=11, penalized close @ 94525** | 44,808 | `3M9b88D…` |

Liquidation tx: https://explorer.solana.com/tx/3M9b88DMiAuKsYFZZwDJM6RQFdMsHpHYFBkn73YMYjNswrLKpXKb5RNRVpJEv91zuD5436Z4sMvsv6ZzEzEQxdmP?cluster=devnet

## 4. Accounting verified exact (quote-lot precision)
Read back from on-chain state after the run:
- **Taker fee**: trader3 collateral = 400,000 − **5,000** = 395,000 (10,000,000 notional × 5 bps). Exact.
- **Maker rebate**: trader2 collateral = 20,000,000 + **1,050** (50 on the 5-lot + 1,000 on the 100-lot fill, 1 bps). Exact.
- **Net protocol fee**: fresh-market `total_fees_collected` = **4,000** (5,000 taker − 1,000 maker rebate). Exact.
- **PnL**: victim 100 × (95,000 − 100,000) = **−500,000** quote-lots → equity negative vs 395,000 collateral → insolvent. Liquidation correctly fired.
- **OI conservation**: long == short on both markets. Exact.

## 5. MagicBlock ER round-trip (verified)
Full delegate → trade-on-rollup → commit → undelegate, on market `AefDtaLHG53cUCXVYXRqiaNssLLSpTWEuoD6xkxQYiZV`:
- Base layer: `delegate_market_book` (CU 41,965) + `delegate_market` (CU 33,288) → CPI delegation program `DELeGG…`.
- ER (`devnet.magicblock.app`): `place_limit_order_v2` BID 3@99000 + ASK 2@101000, `commit_market_book` (24,816), `commit_and_undelegate_market_book` (24,818) → CPI Magic program.

## Known finding surfaced during this run
Calling `initialize_haircut_state` flips `market.haircut_enabled = true` permanently. On a haircut-enabled
market, `apply_fill` then *requires* per-position haircut accounts, but `init_position_haircut_state`
requires the position to already exist — which only `apply_fill` can create. **Opening a brand-new
trader's first position on a haircut-enabled market is therefore a deadlock**, and a taker fill that
gets stuck unsettled blocks the FIFO commitment ring for that market. Workable today only by enabling
haircut *after* positions exist; worth a protocol fix (init position + position-haircut atomically, or
`init_if_needed` the position-haircut inside `apply_fill`).
