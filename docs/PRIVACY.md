# Private (dark-pool) books

A Clober market can run its order book as a **private book**: the
delegated book PDA executes on a MagicBlock **Private Ephemeral Rollup**
(TEE-backed), and an *ephemeral permission* account gates who may read the
ER's state. When a book is private, only allow-listed readers can see depth,
resting orders, and flow; public observers are denied. Settlement is
unchanged — every fill still lands on L1 through `apply_fill`, which
verifies it against the fill-commitment ring
([SETTLEMENT.md](SETTLEMENT.md)), so privacy never weakens settlement
authenticity.

## Design boundary

- **Additive and isolated.** The three privacy instructions are
  authority-gated and touch only the permission account. No matching, risk,
  or settlement path reads or depends on them; a market that never calls
  them behaves identically to one where they do not exist.
- **Enforcement is TEE-side.** The program manages the allow-list; the
  MagicBlock Private ER enforces read gating. On-chain state proves *what*
  the allow-list is, the TEE enforces *that* it is honored. The CPI byte
  construction is fully host-tested; live enforcement is observable only
  against a Private ER.
- **L1 state remains public** (as on every Solana program): positions,
  collateral, and settled fills are on L1. Privacy covers the *pre-trade*
  book — depth, orders, and flow on the ER.

## Instructions

| Instruction | Access | Effect |
|---|---|---|
| `init_book_permission` | market authority | Creates the ephemeral permission for the market's book PDA (public, empty allow-list). Idempotent: a no-op if the permission already exists. |
| `set_book_privacy(is_private, members)` | market authority | Toggles privacy and replaces the allow-list. Private: each member is granted the read flag set (logs + messages + balances). Public: the allow-list is cleared. `members.len() ≤ 32` (`MAX_PRIVACY_MEMBERS`). |
| `close_book_permission` | market authority | Closes the permission, refunding rent to the book PDA. |

The book PDA itself signs each CPI (`invoke_signed` with the
`[b"market_book", market]` seeds) and also pays: the delegated book PDA
carries its own lamports onto the ER. Each call emits a
`BookPermissionEvent { market, market_book, is_private, member_count,
action }` (`action`: 0 = init, 1 = update, 2 = close).

## Wire format (permission-program ABI)

The permission program's ABI is implemented in
`programs/clober/src/er_permission.rs` and every assembled byte is
host-tested.

**Programs and addresses**

| | |
|---|---|
| Permission program | `ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1` |
| Magic program | `Magic11111111111111111111111111111111111111` |
| Ephemeral vault | `MagicVau1t999999999999999999999999999999999` |
| Permission PDA | `["permission:", permissioned_account]` under the permission program (the trailing colon is part of the seed) |

**Instruction data** — an 8-byte little-endian discriminator, then for
create/update the member args:

| Instruction | Discriminator (u64 LE) | Trailing data |
|---|---|---|
| CreateEphemeralPermission | 6 | `EphemeralMembersArgs` |
| UpdateEphemeralPermission | 7 | `EphemeralMembersArgs` |
| CloseEphemeralPermission | 8 | — |

`EphemeralMembersArgs` is a flat (non-borsh) layout:
`[is_private: u8][ (flags: u8, pubkey: [u8;32]) × N ]` — 1 + 33·N bytes.
At the 32-member cap an update instruction is ~1,065 bytes of data, inside
transaction limits.

**Member capability flags** (bitmask): `AUTHORITY = 1<<0`,
`TX_LOGS = 1<<1`, `TX_BALANCES = 1<<2`, `TX_MESSAGE = 1<<3`,
`ACCOUNT_SIGNATURES = 1<<4`. Allow-listed readers receive
`TX_LOGS | TX_MESSAGE | TX_BALANCES`.

**Account order**

Create: `payer (signer, w)`, `permissioned_account (signer, ro)`,
`permission (w)`, `vault (w)`, `magic_program (ro)`.

Update/Close: `payer (signer, w)`, `authority (ro, not signer)`,
`permissioned_account (signer, ro)`, `permission (w)`, `vault (w)`,
`magic_program (ro)` — the protected PDA carries the signer bit in place of
a keypair authority.

## Operating a private market

1. Create the market and book, delegate to a **Private** ER validator.
2. `init_book_permission` (once).
3. `set_book_privacy(true, [reader1, reader2, …])` — the book goes dark to
   everyone else.
4. Rotate readers at any time with another `set_book_privacy` call (the
   list is replaced, not merged).
5. `set_book_privacy(false, [])` to re-open, or `close_book_permission` to
   tear down.
