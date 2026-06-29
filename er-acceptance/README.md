# Live-ER acceptance suite (Tier-2)

Exercises the one thing `solana-program-test` structurally cannot: the real
MagicBlock CPI round-trip. See `../ER_TRUST_BOUNDARY.md` §2–3 for why this is the
*only* part of the ER trust boundary not covered by the unit harness.

## What it asserts

The §3.2 authenticity round-trip on a fresh market (book + commitment ring — the
supported ER config; the deep outbox is L1-only, see Findings):

1. **L1** — init market + book + commitment ring (96-cap).
2. **L1 → DLP** — `delegate_market_book` + `delegate_fill_commitment` (the delegate
   CPI the unit harness can't run).
3. **ER** — rest bids + a taker sweep on the rollup (commitments pushed *on the ER*).
4. **ER → L1** — `commit_*` snapshots; assert the committed ring cursor survived
   (`produced == 4` — fill authenticity persisted through the round-trip).
5. **ER → L1** — `commit_and_undelegate_*` → `process_undelegation` finalizes;
   assert the accounts are back under the program and still valid
   (`from_account_data` accepts — the ER L-2 defense on a real committed state).
6. **PROBE** — assert the 256-outbox ER-delegation is correctly blocked (Finding 1).

## Run

Gated on `ER_RPC` (skips cleanly with exit 0 when unset — like the SBF benches skip
without `BPF_OUT_DIR`, so it never breaks CI):

```
npm install
L1_RPC=https://api.devnet.solana.com \
ER_RPC=https://devnet.magicblock.app \
  npm run acceptance
```

Requires a funded keypair at `~/.config/solana/id.json` that is the market authority,
and the program deployed on the target cluster. This is the **Tier-2 gate** to run
before a mainnet cut; it is not a CI unit test.

## Findings from the first live run (2026-06-29, devnet `magicblock-core 0.13.2`)

The suite earned its place immediately — it found two things no unit/integration
test could (they don't delegate):

1. **VERIFIED: the 256-slot outbox cannot be ER-delegated.** `delegate_fill_outbox`
   creates the delegate-buffer at the full 24,640 B in one CPI, exceeding the
   10,240 B/ix BPF-loader cap → "Failed to reallocate account data." Since the
   matcher needs `fo_cap >= ring_cap` (256), the deep outbox is **L1-only** under the
   current ring cap. Book + ring delegate fine (< 10,240 B), so the §3.2 authenticity
   round-trip works on the ER; the 256 deep-sweep is an L1 feature until the delegate
   buffer is chunked or the ER cap is lowered (see `../FILL_OUTBOX_DESIGN.md` §10).
   The `PROBE` stage asserts this block is in place.
2. **Routing:** `delegate(…, null)` assigns the account to whichever validator claims
   it; the ER match stage must transact against *that* validator (or the MagicBlock
   router). With a bare public endpoint the match can return `InvalidWritableAccount`.
   Pin the validator identity in the delegate calls or route through the MagicBlock
   router for a deterministic green. The delegate-CPI round-trip (book + ring) is
   validated regardless.

**Validated on the live ER:** L1 market/book/ring setup, and the **delegate CPI for
book + ring** (the round-trip start) against the real DLP on devnet.
