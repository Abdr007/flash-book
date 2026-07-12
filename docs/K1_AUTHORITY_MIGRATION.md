# K-1 — Authority Migration Runbook (single-key rug → multisig)

**Status gate:** K-1 is RESOLVED only when `scripts/verify_k1_authority.mjs` exits 0
against the target cluster. It cannot be marked resolved from source — a human
holding the current authority key + the multisig signer set must execute this,
and the verifier must go green on-chain.

## The finding
The deployed program `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` upgrade
authority is a single mutable wallet (`GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD`,
System-owned). One key can `solana program deploy` arbitrary bytecode and drain
every vault — outside all in-program governance (timelock/guardian/burn). The
per-market `authority` and `insurance_fund.authority` (and the sequencer role)
must also leave that single key.

## Target end-state (the verifier's PASS predicate)
1. **Program upgrade authority** == the Squads 3-of-5 vault PDA (or `None` = immutable).
2. **Every `market.authority`** == the multisig (or `default` if `burn_market_authority` was used).
3. **Every `market.sequencer`** != the multisig and != any authority key — a dedicated ops hot key (it signs settlement, not governance).
4. **`insurance_fund.authority`** == the multisig.

## Migration sequence (mainnet; run each on the target cluster)
Signer topology: the CURRENT single key signs steps that transfer *away from
itself*; the MULTISIG signs the `accept_*` steps. Do the upgrade authority and
sequencer separation FIRST (they are one-way / independent), then the 2-step
authority transfers.

```
# 0. Create the Squads 3-of-5; record its vault PDA.
SQUADS_PDA=<vault-pda>;  OPS_HOTKEY=<sequencer-only-key>

# 1. Program upgrade authority → multisig (signed by the current authority key).
solana program set-upgrade-authority 5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq \
  --new-upgrade-authority $SQUADS_PDA --url <RPC>
#    Immutable alternative (irreversible): --final  (no --new-upgrade-authority)

# 2. Per market — separate the sequencer from governance FIRST (so settlement
#    keeps working after the authority moves):
#      instruction: set_market_sequencer(new_sequencer = $OPS_HOTKEY)   [authority-signed]

# 3. Per market — 2-step authority transfer (typo-safe):
#      propose_authority_transfer(new_authority = $SQUADS_PDA)          [current authority]
#      accept_authority_transfer                                        [executed BY the multisig]

# 4. Insurance fund authority → multisig via the same authority-transfer flow
#    on the InsuranceFundAccount authority (or re-init under the multisig at genesis).

# 5. (Optional, per market) once params are final: set_market_status / lock_oracle_source /
#    burn_market_authority — all one-way, executed by the multisig.
```

Rollback safety: step 1 is reversible by the *new* authority (the multisig) until
`--final`; steps 3/4 are 2-step (a mistaken `propose` is cancellable via
`cancel_authority_transfer` before `accept`). Do NOT `--final`/`burn` until the
verifier is green and a rehearsal on devnet passed.

## Verify (the gate)
```
node scripts/verify_k1_authority.mjs \
  --program 5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq \
  --multisig $SQUADS_PDA \
  --markets <MARKET_PDA>[,<MARKET_PDA>...] \
  --insurance <INSURANCE_FUND_PDA> \
  --url <RPC>            # add --immutable instead of --multisig if you chose --final
```
Exit 0 = K-1 RESOLVED; exit 1 = still FAIL (prints the exact failing check). This
script reads the live chain (`solana program show` + `solana account` decoded via
the committed IDL offsets); it is the ONLY thing that flips K-1 to resolved.

## Certificate wording
Until the verifier exits 0 against the launch cluster, the production-readiness
certificate remains **FAIL on K-1**. Do not overclaim: the runbook + verifier are
READY; execution requires the human authority-key holder + the multisig signers.
