# Live-ER acceptance suite (Tier-2)

Exercises the one thing `solana-program-test` structurally cannot: the real
MagicBlock CPI round-trip. See `../ER_TRUST_BOUNDARY.md` §2–3 for why this is the
*only* part of the ER trust boundary not covered by the unit harness.

## What it asserts

The FULL off-log round-trip on a fresh **cap-105 (ER-capable)** market — book +
§3.2 ring + fill-outbox, all one-CPI delegate-safe at this cap:

1. **L1** — init market + book + ring (`init_fill_commitment(105)`) + the FULL outbox
   (`init_fill_outbox` reads the ring cap → 105 slots in one ix, no grow).
2. **L1 → DLP** — `delegate_market_book` + `delegate_fill_commitment` +
   **`delegate_fill_outbox`** — the whole pipeline delegates (the versatile win; at
   cap 256 the outbox can't, see Findings).
3. **ER** — rest bids + a taker sweep on the rollup (commitments pushed + outbox
   written *on the ER*).
4. **ER → L1** — `commit_*` snapshots; assert the committed ring AND outbox cursors
   survived (`produced == 4` each).
5. **ER → L1** — `commit_and_undelegate_*` → `process_undelegation` finalizes;
   assert all three are back under the program and still valid (`from_account_data`
   accepts — the ER L-2 defense on a real committed state).

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

## Findings (devnet `magicblock-core 0.13.2`)

The suite earned its place immediately — it found two things no unit/integration
test could (they don't delegate):

1. **The 256-slot outbox (24,640 B) cannot be ER-delegated** — `delegate_fill_outbox`
   creates the delegate-buffer at the full size in one CPI, over the 10,240 B/ix
   BPF-loader cap. **RESOLVED → versatile per-market cap:** the cap is set at
   `init_fill_commitment(cap)`; at **`cap ≤ 105`** both the ring and the full outbox
   are one-CPI delegate-safe, and the suite's "delegate book + ring + OUTBOX" stage
   now **PASSES on the live ER** (verified). `cap` up to 256 is the L1 deep-sweep. So
   one mechanism serves both environments (see `../FILL_OUTBOX_DESIGN.md` §8, §10).
2. **Routing (open):** `delegate(…, null)` assigns the account to whichever validator
   claims it; the ER match stage must transact against *that* validator (or the
   MagicBlock router), else it returns `InvalidWritableAccount`. Pin the validator
   identity in the delegate calls or route through the MagicBlock router for a
   deterministic match-stage green. The delegate-CPI round-trip is validated
   regardless.

**Validated on the live ER:** L1 setup of the full cap-105 pipeline, and the
**delegate CPI for book + ring + outbox** against the real DLP on devnet.
