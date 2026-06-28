# Flash Book — live devnet transactions

**47 distinct instruction types landed on-chain.**

Program (anchor, deployed): `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq`  
Signer/authority: `GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD`  
Demo market: `3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q`  
Cluster: devnet

### Account & protocol setup

- `open_trader_state` — https://explorer.solana.com/tx/4bDtTbJGNHKyshgSeCskLPJz52KNVuTr2XaRJiLsefvnAjfsNVsYAtvWSrz93oLvwoJXVwbDhzA29NR68VMgDc4P?cluster=devnet
- `initialize_insurance_fund` — https://explorer.solana.com/tx/5YrQgVa647K1MDKWwjREFuWhH4fJnAwSrXQ1FJZ9Si8M58T1bVFdEpy9UgZU7JVMwyT6d3qWoecamxTNZdvY3q9L?cluster=devnet
- `initialize_flp_exposure` — https://explorer.solana.com/tx/4vaVWuwEp25t5s6c76MhujAyCJ21aWcgcURLQNtMRn3KNtSSCfm6q2rLRgowTiJw3R9CL369RoEnjZxaKx4EFnYF?cluster=devnet
- `init_fee_tiers` — https://explorer.solana.com/tx/3E9Fn3XFXH2n3hwjbd7NPykLJWo1aXauy6AiYr26B3WNwoFc3puKRv7b6WUvh8jvBWH2riV9UGBGAvUVSUNtLUMt?cluster=devnet

### Market creation

- `initialize_market` — https://explorer.solana.com/tx/5da4bo1HRMykcPF6JLi8dergxcHYBzrV13VNpQpxrcDe2wq5ktvMRErqmJJpeaQoMHEg3fuGEtgtRBQJfi812QEH?cluster=devnet
- `init_market_book` — https://explorer.solana.com/tx/4bWQmv1mNqnioyyRJQN7Ug7vZLJVWMcA23AUYGwHbz59btKXUHcjUjdgebAMmT7z3kPHJJ2npcPtbXbbCR3ZqcXE?cluster=devnet
- `expand_market_book` — https://explorer.solana.com/tx/3vm9EtZd6NPDRzLnXs5WF3GHP51cYcKDBsErP3Qiq7TMaEMBz1CbVnp8qkxpLjMF6JVvA9aTvu2pnRnuc5Ywjsvq?cluster=devnet
- `init_market_leverage_tiers` — https://explorer.solana.com/tx/L3QGAFaQcxP6dWBKaFfjMJuWhv9eGPRhhQBCzPaQVpBNfpAPjC3KjmJSwWqTRrS6Uzpm5gXLhC8NHo23LxFEhBc?cluster=devnet
- `init_market_oracle_config` — https://explorer.solana.com/tx/2iGmFAKbuDwc6vbSgAV4LB31BQJDn99VHbgvfk2HonoEVSfrNhCV7TaPxw4uMDTTzEUuX1CEUicwFGFXVgnMAc3Y?cluster=devnet

### Oracle & risk envelope

- `set_envelope_config` — https://explorer.solana.com/tx/GDWVKAa7D15urnQcqjgd64BazNy5Ys8xfhJUJhgdJKzXPaW1UsVcezoBbNA9UiLSHPwttJqjbzdMQymmqoZXv5z?cluster=devnet
- `verify_envelope_config` — https://explorer.solana.com/tx/3VjNeGACVVxdWJdarKcTw3JYNAqkiegccW9Y2P2hXet2LHZa2mzjZjeRGbMx7vtDjfJsgQ1KBfPR3dhJjJ89awYg?cluster=devnet
- `gate_envelope_price_move` — https://explorer.solana.com/tx/4Qeu3tDqfiFTiTbhaFaX6WxFYM5jmrToJyTPXFJNuFW2a5ijLZ4XPqHYn55CJgxybA6JvwPCKmnjuwXRadX3ZtaA?cluster=devnet
- `update_oracle` — https://explorer.solana.com/tx/55SEmZPG5cX43opJbHm4ioWJ6MN78Mik9v2FRqgUNKSUafgSwQ8GWVbPjvuqR3X67xV5MgsQnQ32aUynhBXQfKyu?cluster=devnet
- `update_oracle_quorum` — https://explorer.solana.com/tx/3YvdZWT2TVMsbcycdEcBPiFM1iZ9FsBzRYKNNstbGuxTkKy29ynTmXG43UneXmxzBxa1KjcwvdQAvBxtfYRULsaZ?cluster=devnet
- `settle_mark` — https://explorer.solana.com/tx/5LQsFiDXk5ifogWNZqHgxQPYyHa93droXAmYR9ySU2dEBWSDjHYM9Jmm8UzZiuqeGe14rkT1Rff3P8eXMJQwS448?cluster=devnet
- `verify_market_invariants` — https://explorer.solana.com/tx/43SQp5uJF8aScAgpq3cTnghmDcAvY1ZNEygdH5LxcYqJtS648yKszCCUdk7uZVh4mG3UL7tfjzjaLrZVfkU7j6SW?cluster=devnet

### Market admin

- `update_market_params` — https://explorer.solana.com/tx/5Ur6fBE23ofdieBGzDLAqV7db2E9gJGNJxdzkSQ6v95iZsAPkwQQSDEELAFPzwUPPx1Y6wRBgPjwYzoymg6d3m7H?cluster=devnet
- `set_market_sequencer` — https://explorer.solana.com/tx/4QVSKywApU8Lvqe2EKad4pNBndbhvvPkd9MhhSnQ2Bj9yuLM3bULqEnhGmhEBg3FPYXRbGLpJxVXLA8NJV1SeUVL?cluster=devnet
- `set_market_status` — https://explorer.solana.com/tx/36JFXPwaEyQz56KBbpaE3Tz4WssHJLrbrMnd71fyPxEXKbAXUYczXJf4kzkwPYNRTwb1UHXCbZ76TMSuaQgWnsCG?cluster=devnet
- `transfer_market_authority` — https://explorer.solana.com/tx/im3ywQhijYaBG8NdPpShqCohsrfXvMnmXtGpk6A2LCPA5PLhFPmsF78imtKjG6WBQGSuEph4U4GfLxs9YMndZym?cluster=devnet

### Trader account ops

- `open_trader_sub_account` — https://explorer.solana.com/tx/3zfW9nxVHU9gtZuWH6XtriH9vo5cVu1iqdjPh1JmpKscEqyaDswzv6a3AJWWyScyjCj5YUvEDv5VAVYcwi7GtCCQ?cluster=devnet
- `init_trader_ata` — https://explorer.solana.com/tx/2exBBEXzsV5AjWPAe1JxjWcJ3Lynqku5m5WtNGzwNsUTV3Svw2RjdcpSqwNHss39eXu6rJUbmZcsvNfZ5uTaLHoP?cluster=devnet
- `set_trader_referrer` — https://explorer.solana.com/tx/5fHNsM7UkkGL3rNQeTmYUk7eQQLdCdaoNPHnjsb9ATUo1hSrjM11qDkBSFt4vmyXaLt7FfJLnqcVFppgD1ST86Je?cluster=devnet
- `set_trader_delegate` — https://explorer.solana.com/tx/3yLxnUSUY4sWxMyu4PWpcb4BX9fF1tqX1xBJqTm99Z7mGV6xSWrw6Z82XjC3vgD8aSeUGKk533vKxDnqUq7uCZwK?cluster=devnet
- `set_trader_builder` — https://explorer.solana.com/tx/2j7cBY881oaehtYEgXEk2DNbrQT8LFaVyDNaMGR2PamYdgwcaDfMCzov4GMkxAyVu62Kgq5N656e7zveqwmVGpBL?cluster=devnet
- `set_trader_fee_tier` — https://explorer.solana.com/tx/5CUo6e5SA7g9UUvAPgDpoLHeU4SgXnyXfkvMYFVsY9eiPBkkfKXaFJRDnQo1898jVrNjzqo4N6qrcMRhpnnmiw2f?cluster=devnet
- `create_session_token` — https://explorer.solana.com/tx/2dYkmTFcsVTgAfAckBhmhAyTsJFQhjym5twzDVUVmzwNMEpfuYBwH749qS81rcX5gTU373KtsHxJLTJXasqHAS1b?cluster=devnet
- `revoke_session_token` — https://explorer.solana.com/tx/5eKUW7NZgYvqFLnG1uNhxRn3Eeo64HRpmKMtrjT2ZG5wLJhekDuAfpMpXG1BiA6RY91gpads8Uqo7EgKYWS9ndES?cluster=devnet

### Collateral

- `deposit_collateral` — https://explorer.solana.com/tx/2U5NRNF77rw3QiQZKdB2j8pbSyE7TK1sXr3nRcw3ELh73w9NJfe3JTnBBzQt5ihkpV5fGi64sMJpu3DAyTC52Agk?cluster=devnet
- `withdraw_collateral` — https://explorer.solana.com/tx/2nNrqA6wGxg6ZeA41atJxqDKCWvvvbJKXwghT8NPTp6GVo8KFiHzuFwHZaPatb76Jn23jd6RmSsDcZ2U72po9QY1?cluster=devnet
- `transfer_main_to_sub` — https://explorer.solana.com/tx/4WcWVg4CxCcQ7qJPtepmKTdofjfcpmzf6qkFmPzqg8oCZxrVCgS16KNmWTsc6rCHGR5aiBsygfeva8L9F4KnYN2r?cluster=devnet
- `transfer_sub_to_main` — https://explorer.solana.com/tx/3QZtV4B1iSWcTjiTyqdDqrwBc31zURv5n1EtT9cBjRX4LQWhzYXcvpNj4utFkkaepxAKkwAcgFqJ7YZoH7AkXiHW?cluster=devnet

### FLP (pool liquidity)

- `deposit_flp_capital` — https://explorer.solana.com/tx/eUa9HVTX9qrYhmFykayMbUDmgGXLdA6hY2jomtDQtk7rUDif1EtB3eiaH5pVeRupMLQG9QLCDQHNYZRQ1QYk6DK?cluster=devnet
- `withdraw_flp_capital` — https://explorer.solana.com/tx/VsUJiUmhSHVU67tpQWAscFtGenwPU3Q5c6TFyX2W8YTXjM844rAj3MrPu4dwWX8PqBZNWxFntaBCrncSTm7h2yH?cluster=devnet

### Order lifecycle (CLOB)

- `place_limit_order_v2` — https://explorer.solana.com/tx/3hGmZ326TFSwtEF1Hi7wSYT1nPH1BYBvUQyJvXYN88KSXtW18CtUgnHHLxv5PTuzLAoihUFptVW7s2D9km6WCy13?cluster=devnet
- `modify_order_v2` — https://explorer.solana.com/tx/wZ1o8EF8MHNHQF5xKf9jNZg1a5AhpUhsgtz3rAJ5EH3LErLQs2MywLco7V9K2qa8bUqx6nA3GqCPMp89uro6QCC?cluster=devnet
- `cancel_order_v2` — https://explorer.solana.com/tx/T5NsSTP64b8Ed2PUAXXipmJd6bXjcyi2ANYL9TGjobtwYgQkuLmFbCyyhb6LtBeoVSv4cForfKT6EFY1SXpsCSD?cluster=devnet
- `cancel_all_v2` — https://explorer.solana.com/tx/2xJHpwUYyudW8Wc8jrnZstgBeSFbRjEEPdoAUBHj4URer3Nx6wnNA5WyuujJ6pGdbkUwnehBmSHPJ74ZcUbM4bv2?cluster=devnet
- `reap_expired_orders` — https://explorer.solana.com/tx/4UXjZN5WcFPW7HkjQH8MSzXj44MmrfTFHcadN1DQYB8TA7iw7vz3DjCwFUuukrCKYnRZw86WK98jpj68LWifCkoY?cluster=devnet
- `place_taker_order_v2` — https://explorer.solana.com/tx/3xutAwQ1BoPNCeyqtMAv6kyRitHWMtjqjR9ZkarJbWehaAuoeKo5zg85wVALtTdDjN7Sjd6BcWEdnMkHSGxF9ywn?cluster=devnet

### Read-only views

- `view_portfolio_risk` — https://explorer.solana.com/tx/2CoouaCfcjVxtp91Uz5s2GVgy7ZhNzUTDm8r6AasJVMjT6s9WsebPYY6EpV46jzX8DC2izu2ZRDaiQFNncAiNzi8?cluster=devnet
- `view_trader_effective_tier` — https://explorer.solana.com/tx/5nnAo3zuP2BtJiArRFfivjmDy4Nj9BZucZ33H3cXfzcHZT7ui3LRAZxSVVwq6HrTD2PaYz5AiPPNpeTpztewCAUi?cluster=devnet
- `view_book_depth_v2` — https://explorer.solana.com/tx/5muRiC7xCSyBtehGjnkXh8AALJTwYHR5vfDPLeJNU62kVdRuGtWNBAuVLyGJM8RzwLBaHRCUQ6r6TWe5NzsV9hwk?cluster=devnet
- `view_predicted_funding` — https://explorer.solana.com/tx/53jRxNZ2Rzvn4Rfvv56yV5ARUENTGotZS8cqynEaybCCyxxAZ5RvH5nzGaXfKy5khGbobP5p9M39S8oH6rkrUVF8?cluster=devnet
- `view_quote_ladder` — https://explorer.solana.com/tx/5LAzYEPQJk8rhWYx4yBSt9VrrGNwtzeSi3vtuRDd1v8SQDExeXWmFQ5Wsn2S3RATnxiep47msp4QMSKxDCasunAB?cluster=devnet

### ER (base-side)

- `er_heartbeat` — https://explorer.solana.com/tx/2mJZET1iELNFyGM8GLvmrjUGc6fGNGTEUYSBbRQSjJCZNqsR4AgDomf5jkTFWJzmGnWfzazUyTFR1oNAt4F5JcUR?cluster=devnet
- `init_fill_commitment` — https://explorer.solana.com/tx/2DoGyi6QRZ8M86zu2z5m6KgwuC7QTdKwKb2ZyVVjt8u1NwkzcnTBYPhgU177mj2mvKW1p6eQHJvL193kz2VGhVMS?cluster=devnet


---

## Program upgrade — MagicBlock ER delegation FIXED (2026-06-28)

Rewrote `er::cpi_delegate` to the current MagicBlock delegation-program ABI
(buffer stage + copy + zero + owner-reassign + 8-byte discriminator + buffer close),
rebuilt with platform-tools v1.52 (cargo 1.89), extended the program account, and
redeployed.

- `program upgrade (deploy)` — https://explorer.solana.com/tx/4YDRN4Ve2CMQ9BPWPa8Y3QC9erJhfkted2pxUAc5ZWhKRG2SgeEFzsi83Cu3wZoqx3APg268q56atxVBEMn6fnND?cluster=devnet

### ER delegation lifecycle — now working on-chain
- `delegate_market_book` (book → ER; base-layer owner becomes the dlp) — https://explorer.solana.com/tx/4idMKqzD3fiWbHwfYoX6Mo3Nq9hfSDwSeHacrrq7RCFsHQwoxycTVAW8bga5KRVKSF7tpvybKLHo6wZdZSuHjbkz?cluster=devnet
- `commit_market_book` (ON the ER) — https://explorer.solana.com/tx/2Xf5oSAJ5kkQDSwsFqkpVcqsvBDZx5UwiwQDzdvyM6vjcwauRq7buAUtbSf9BgPcordaaDMadBPYtniQigUD9QtR?cluster=devnet
- `commit_and_undelegate_market_book` (ON the ER; ownership returns to program) — https://explorer.solana.com/tx/6518cb8f9YkULSoiwF2qvHNQa45xq4hzrcdz44xeF8WXkoKMDjknNFVA9pKTGP88ZgEWhgR6oXP54cd6yxccYKXC?cluster=devnet

### Second clean run (fresh market A3m18dFP…)
- `delegate_market_book` — https://explorer.solana.com/tx/58E8PqepnUWbQzMK3Ff9cYWrQLVnJ3YphqFdyD74yex4cxmbeteyza9bY1E9pa6QPeh3BbZrvumdHtbJA2JRuVhB?cluster=devnet
- `commit_market_book` (ER) — https://explorer.solana.com/tx/5i3QiMUqk58Ezoe3khFxPNtKi35hV8NDceVn8oXFCX9zK6rvHk74ZAiiRwWxo2UpYVy8k6NTL1LQFWYLzigNAkU5?cluster=devnet
- `commit_and_undelegate_market_book` (ER) — https://explorer.solana.com/tx/62np7Y1dzTmvkYqh4581sLsrYw9dFLXeLFyigBiaPVZkLYe9vadmtpFP4tuDrWF1Wu8ykYZFhskYthNXc5e7zcdL?cluster=devnet

---

## COMPLETE ER TRADE — limit orders executed ON the MagicBlock ER (2026-06-28)

Second program fix: `delegate_market` now takes `market` as `UncheckedAccount`
(in-handler owner/authority/PDA validation) so Anchor doesn't re-serialize it
after ownership moves to the delegation program. Rebuilt + redeployed.

- `program upgrade #2 (delegate_market fix)` — https://explorer.solana.com/tx/5Fbcrp5dJMbZtSvFbinHvt6p1ZE2WoU5DoktYwJ5pRXpmGyjmEUVeZrUGmxPAQAqKgsGJka5rxNocodnSWxSkeii?cluster=devnet

### Base-layer (standard devnet explorer) — market AefDtaLHG53cUCXVYXRqiaNssLLSpTWEuoD6xkxQYiZV
- `initialize_market` — https://explorer.solana.com/tx/3VzSDsEqqh5kWRLj4ZhhKzZrEgvyfqxkUjGruNsmzrT6LEnXX9s6ptJtCmnmKgRYpRpQGPZmJA8HAP2wCf7jvwN9?cluster=devnet
- `init_market_book` — https://explorer.solana.com/tx/5MxCtfxNk9iaFY5VxSkRJRsjirNiFJjBzBtagGZedFZzZdUxtA21ywcsCWB3g2vCKsGav9MDBNCNurQJ6AT4PBEA?cluster=devnet
- `set_market_status` — https://explorer.solana.com/tx/23EEQcWJf3CxyUc49KUW5CGBtZcti7vxovpZqfLY4HhMFvRzovMdzLzFaAxmPFf6m5YMYRADp4ctnvYkSC7s7LL1?cluster=devnet
- `delegate_market_book` — https://explorer.solana.com/tx/uDNsr9K1YTxBKwrG24a2k7P3TEZ7wANREt5ovyZsGoea5RnkAdpLXNSn1TDAss2zTHXMqV1tLn2idMC7fqQNUA1?cluster=devnet
- `delegate_market` (the fix) — https://explorer.solana.com/tx/4tgzF9wNakfotHx4JNjJGMUxB45oeV7qgjT8Z6wZZV5GiVPMHzd3Qm9UXfXHeFuSfmNkyUPLPSWkrDVz5oQrFDdH?cluster=devnet

### ON the MagicBlock ER (custom RPC explorer; verified err:None)
- `place_limit_order_v2` (ER, BID) — https://explorer.solana.com/tx/nY7EsR17Wb9TMr2g5tByQzPKf8HcHcEKhjGArZWBXumW1PdyfjgM6q13sNy6kZNTt4K8nRRngQVDDRuKn9iza3q?cluster=custom&customUrl=https%3A%2F%2Fdevnet.magicblock.app
- `place_limit_order_v2` (ER, ASK) — https://explorer.solana.com/tx/2qnD7RFGfWmXZat4zcA9F7ZCxyW9usJy7brEE468NXo26zbc6CeQQBgZxM4dEne8mSCZE4nN26MmzhU5qfhVHWQ4?cluster=custom&customUrl=https%3A%2F%2Fdevnet.magicblock.app
- `commit_market_book` (ER) — https://explorer.solana.com/tx/4tVaGtdJEhpsStsu75wxG7e6iF46wRkUn4fikQqtjBYtc3aWiRedDFPoekN2WQF8P1GfQrUnxa8UvkLvehoZk5Va?cluster=custom&customUrl=https%3A%2F%2Fdevnet.magicblock.app
- `commit_and_undelegate_market_book` (ER) — https://explorer.solana.com/tx/5zNX8vniZ3uo2H1EzViberypfuB2WagDnyxEFbyqVboDrcGn8zoPmD8cK5XhxUZCGBsyMfouWmEHLPLeNEWTvGhT?cluster=custom&customUrl=https%3A%2F%2Fdevnet.magicblock.app
