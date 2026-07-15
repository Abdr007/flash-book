# License Analysis — MIT first-party code + GPL-3.0 vendored hypertree (6.3)

> **Not legal advice.** This is an engineering analysis of the license
> obligations, written to inform the owner's decision. Confirm with counsel
> before relying on it for a public launch.

## The facts

- The repository is **MIT** (`LICENSE`, "Clober contributors").
- `programs/clober/src/hypertree/` is **vendored from Manifest**
  ([Bonasa-Tech/manifest](https://github.com/Bonasa-Tech/manifest)) and carries
  **GPL-3.0-only** (`LICENSE-HYPERTREE`). This is already disclosed in `README.md`.
- The hypertree is **compiled and statically linked into the same on-chain
  program** (`clober`) as the first-party code — one `.so`, one address.

## The legal consequence

GPL-3.0 is a **strong copyleft** license. When GPL-3.0 code is combined with
other code into a single program (static linking into one binary is the
canonical "combined work" / "derivative work" case), the GPL's terms attach to
**the whole combined work upon distribution**:

- **The compiled `clober` program (the distributed `.so`) is effectively
  GPL-3.0-only.** You cannot ship the binary under terms more permissive than
  GPL-3.0, because it embeds GPL-3.0 code you don't own the copyright to.
- **Distribution triggers the source-availability obligation** for the *entire*
  combined work (GPL-3.0 §6): anyone you convey the binary to must be able to
  get the corresponding source under GPL-3.0.
- **No additional restrictions** may be imposed on the GPL portion (§10) — e.g.
  you cannot relicense the hypertree itself to MIT.

The MIT license on the first-party modules is **not wrong** — MIT is
GPL-compatible, so MIT code *may* be included in a GPL work. But MIT is
permissive, not copyleft: it does not *shield* the combined work from the GPL.
The net effect is a **dual-license reality**:

- **First-party modules, taken in isolation:** MIT (you own them; you may also
  offer them under GPL, or extract and relicense them).
- **The combined, distributed program:** GPL-3.0-only.

"On-chain distribution" — deploying the `.so` to a public cluster where others
fetch/execute it — is best treated as distribution for this purpose. The
conservative, launch-safe reading is: **the deployed program is GPL-3.0.**

## Compliance status (against GPL-3.0 as it stands today)

| Obligation | Status |
|---|---|
| Preserve the GPL license text + copyright | ✅ `LICENSE-HYPERTREE` present |
| Disclose the GPL component + its origin | ✅ `README.md` "License" section |
| Corresponding source available to recipients | ✅ public repository |
| No additional restrictions on the GPL portion | ✅ none imposed |
| State significant modifications to the GPL files | ⚠️ **Action:** add a short NOTICE / per-file modification note where the vendored hypertree was changed (GPL-3.0 §5a) |

The one gap is the §5a "carry prominent notices stating that you changed it"
obligation for any local edits to the vendored files.

## Options & recommendation

1. **Embrace GPL-3.0 for the program (recommended for launch).** License the
   *combined work* GPL-3.0-only and keep the first-party modules dual-offered as
   MIT (so integrators can reuse them standalone). Concretely:
   - Change the top-level statement to: *"First-party code is MIT; the compiled
     program, which links the GPL-3.0 hypertree, is distributed under GPL-3.0-only.
     See `LICENSE` (MIT), `LICENSE-HYPERTREE` (GPL-3.0)."*
   - Add the §5a modification notice to any edited hypertree files.
   - This is honest, launch-ready, open-source-friendly, and costs no
     engineering. It aligns with "run it, read it, break it."

2. **Go fully permissive** — replace the vendored hypertree with a first-party
   or MIT/Apache-licensed order-book structure. This removes the copyleft
   entirely but is a **multi-week, high-risk rewrite** of the most
   formally-verified data structure in the codebase (the hypertree book). Not
   recommended before launch; a viable long-term path if a permissive-only
   license becomes a business requirement.

3. **Process/crate isolation** — does **not** apply here. On Solana the program
   is one linked binary; there is no separate-process or dynamic-linking boundary
   that would keep the hypertree at arm's length. Discard this option.

**Recommendation: Option 1.** It is accurate, compliant with one small NOTICE
addition, and imposes zero engineering cost or launch delay. The project is
already open-source and public, so GPL-3.0 on the combined program changes
nothing operationally — it only makes the licensing statement precise.

## The one code action (independent of the decision)

Add a `NOTICE` (or per-file header) recording that `programs/clober/src/hypertree/`
is a **modified** vendored copy of Manifest's GPL-3.0 hypertree, listing that
local changes were made (e.g. the `certora`/`kani` cfg gates, `NIL` bound). This
satisfies GPL-3.0 §5a regardless of which licensing option is chosen.
