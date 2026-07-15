// Release gate for the public transaction interface. This complements IDL-drift:
// it rejects incomplete, ambiguous, or undocumented instruction metadata before
// clients, indexers, and Explorer users consume it.

import { readFileSync } from "node:fs";

const [,, path = "idl/clober.json"] = process.argv;
const idl = JSON.parse(readFileSync(path, "utf8"));

function fail(message) {
  console.error(`IDL SURFACE ERROR: ${message}`);
  process.exitCode = 1;
}

function unique(items, label, key) {
  const seen = new Set();
  for (const item of items) {
    const value = key(item);
    if (seen.has(value)) fail(`duplicate ${label}: ${value}`);
    seen.add(value);
  }
}

if (idl.metadata?.name !== "clober") fail("metadata.name must be clober");
if (!Array.isArray(idl.instructions) || idl.instructions.length === 0) {
  fail("instruction list is empty");
} else {
  unique(idl.instructions, "instruction name", (ix) => ix.name);
  unique(idl.instructions, "instruction discriminator", (ix) => JSON.stringify(ix.discriminator));

  for (const ix of idl.instructions) {
    if (!/^[a-z][a-z0-9_]*$/.test(ix.name)) fail(`invalid instruction name: ${ix.name}`);
    if (!Array.isArray(ix.discriminator) || ix.discriminator.length !== 8) {
      fail(`${ix.name}: discriminator must contain 8 bytes`);
    }
    if (!Array.isArray(ix.docs) || ix.docs.length === 0 || !ix.docs.join(" ").trim()) {
      fail(`${ix.name}: missing public documentation`);
    }
    for (const account of ix.accounts ?? []) {
      if (!/^[a-z][a-z0-9_]*$/.test(account.name)) {
        fail(`${ix.name}: invalid account name ${account.name}`);
      }
    }
    for (const arg of ix.args ?? []) {
      if (!/^[a-z][a-z0-9_]*$/.test(arg.name)) fail(`${ix.name}: invalid argument name ${arg.name}`);
    }
  }
}

for (const [label, items] of Object.entries({ account: idl.accounts, event: idl.events, type: idl.types })) {
  if (!Array.isArray(items) || items.length === 0) fail(`${label} list is empty`);
  else unique(items, `${label} name`, (item) => item.name);
}

if (!Array.isArray(idl.errors) || idl.errors.length === 0) {
  fail("error list is empty");
} else {
  unique(idl.errors, "error code", (error) => error.code);
  unique(idl.errors, "error name", (error) => error.name);
  for (const error of idl.errors) {
    if (!error.msg?.trim()) fail(`${error.name}: missing user-facing error message`);
  }
}

if (process.exitCode) process.exit(process.exitCode);
console.log(
  `Validated IDL surface: ${idl.instructions.length} instructions, ${idl.accounts.length} accounts, ` +
    `${idl.events.length} events, ${idl.errors.length} errors.`,
);
