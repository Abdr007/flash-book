// IDL-drift gate. Compares two Anchor IDL JSON files by CONTENT (deep key-sorted
// canonical form), so ordering / whitespace differences from separate
// `anchor idl build` invocations do not cause spurious failures — only a real
// divergence (an added/removed/changed instruction, account, arg, discriminator,
// event, type, error, or doc string) fails the gate.
//
//   node scripts/check-idl-drift.mjs <committed.json> <freshly-built.json>
//
// Exit 0 when identical, 1 when they diverge (printing the first differing
// top-level section and, for instructions/accounts/types/errors, the first
// differing named item).

import { readFileSync } from "node:fs";

function canon(x) {
  if (Array.isArray(x)) return x.map(canon);
  if (x && typeof x === "object") {
    const o = {};
    for (const k of Object.keys(x).sort()) o[k] = canon(x[k]);
    return o;
  }
  return x;
}

const [, , committedPath, freshPath] = process.argv;
if (!committedPath || !freshPath) {
  console.error("usage: check-idl-drift.mjs <committed.json> <fresh.json>");
  process.exit(2);
}

const committed = JSON.parse(readFileSync(committedPath, "utf8"));
const fresh = JSON.parse(readFileSync(freshPath, "utf8"));

if (JSON.stringify(canon(committed)) === JSON.stringify(canon(fresh))) {
  console.log("IDL in sync: committed matches a fresh anchor build.");
  process.exit(0);
}

console.error("IDL DRIFT: committed IDL diverges from a fresh anchor build.");
console.error(`  committed: ${committedPath}`);
console.error(`  fresh:     ${freshPath}`);

// Localise the drift to a section and, where the section is a named list, item.
const namedListKeys = new Set(["instructions", "accounts", "types", "errors", "events"]);
for (const key of new Set([...Object.keys(committed), ...Object.keys(fresh)])) {
  const c = JSON.stringify(canon(committed[key]));
  const f = JSON.stringify(canon(fresh[key]));
  if (c === f) continue;
  console.error(`  section "${key}" differs`);
  if (namedListKeys.has(key) && Array.isArray(committed[key]) && Array.isArray(fresh[key])) {
    const fByName = new Map(fresh[key].map((i) => [i.name ?? i.code, i]));
    const cNames = new Set(committed[key].map((i) => i.name ?? i.code));
    for (const item of committed[key]) {
      const nm = item.name ?? item.code;
      const other = fByName.get(nm);
      if (!other) {
        console.error(`    - "${nm}" present in committed, absent in fresh`);
      } else if (JSON.stringify(canon(item)) !== JSON.stringify(canon(other))) {
        console.error(`    - "${nm}" changed`);
      }
    }
    for (const item of fresh[key]) {
      const nm = item.name ?? item.code;
      if (!cNames.has(nm)) console.error(`    + "${nm}" present in fresh, absent in committed`);
    }
  }
}
console.error("\nRegenerate with: anchor idl build -o idl/flash_book.json");
process.exit(1);
