# Attestation cranker

The sequencer-side half of the withdraw-anytime model
([ER_TRUST_BOUNDARY.md §1.2](../ER_TRUST_BOUNDARY.md)). Collateral is
authoritative on L1 while resting orders live on the delegated ER book, so the
margin a live order reserves reaches L1 only through the sequencer-signed
`attest_er_reserved_margin`. This service closes that loop continuously —
bounding the documented attestation-lag window to roughly one poll interval.

## What it does

Every `INTERVAL_MS` (default 2s), per watched market:

1. Snapshots the `market_book`, fill-commitment ring, and fill outbox in one
   `getMultipleAccountsInfo` call — from the ER when delegated, from L1
   otherwise — and walks both order trees of the hypertree slab directly
   (no events, no cache: the book itself is the source of truth).
2. Computes each trader's reservation:
   `im = ceil(size_lots × price_ticks × tick_size × initial_margin_ratio_bps / 10_000)`
   summed per `(trader, sub_index)` → trader_state over **both** live
   surfaces: every resting order, and every unsettled fill (outbox rows at
   absolute indices `[ring.settled, ring.produced)`, reserved for the taker
   and the maker; FLP virtual-quote fills reserve only the taker side). A
   fill's margin therefore stays reserved seamlessly from the moment the
   order rests until `apply_fill` settles it into a position the
   filled-position gate covers.
3. Diffs against the on-chain `ErMarginAttestation` and attests any change
   with the next epoch (strictly increasing, replay-proof). A trader whose
   orders and fills are all gone is attested back to 0, which clears
   `er_active` and re-opens the plain withdrawal paths.

Markets that don't exist yet load lazily — the cranker can be pointed at a
market that is created after it starts.

On startup it reconciles from chain — every attestation account with a
nonzero reservation joins the tracked set — so a restart still zeroes
reservations whose orders are gone.

## Running

```
L1_RPC=<l1-rpc> ER_RPC=<magicblock-er-rpc> \
MARKETS=<market1,market2,…> \
node attestation_cranker.mjs
```

Optional: `KEYPAIR` (default `~/.config/solana/id.json` — must be the pinned
attestor of every maintained `ErMarginAttestation`), `INTERVAL_MS` (default
2000), `ONCE=1` for a single pass.

## Acceptance

`cranker_acceptance.mjs` proves the production loop live against the
MagicBlock devnet ER with **zero manual attestations**: rest orders on the ER
→ the cranker converges the attestation to the exactly-computed reservation →
strict withdraw fails closed → over-withdraw rejected → free balance
withdrawable mid-session → the taker consumes the orders → the reservation
persists as unsettled-fill margin for BOTH sides and the last lots stay
locked → on an L1-resident second market the same fills settle via
`apply_fill` and the cranker releases exactly the settled market's
reservation while the delegated market's unsettled margin persists. Gated on
`ER_RPC` like the rest of the acceptance suites:

```
L1_RPC=<devnet> ER_RPC=https://devnet-as.magicblock.app node cranker_acceptance.mjs
```

## Boundaries

- The cranker trusts the ER RPC it reads the book from — the same
  single-sequencer trust the attestation itself already embodies (§1.2). A
  malicious ER can under-report the book to its own attestor; that collapses
  into the accepted single-operator assumption, not a new one.
- One attestation account per trader_state spans **all** markets, so a
  deployment must run a single cranker instance (or shard by trader, never by
  market) to avoid two writers ping-ponging epochs.
