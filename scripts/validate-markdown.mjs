import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const markdownFiles = execFileSync("git", ["ls-files", "*.md"], {
  cwd: repoRoot,
  encoding: "utf8",
})
  .trim()
  .split("\n")
  .filter(Boolean)
  .filter((relativePath) => existsSync(resolve(repoRoot, relativePath)));

const prohibitedTerms = [
  /\bflash[ _-]?book\b/i,
  /\bflash[ _-]?trade\b/i,
  /\bpercolator\b/i,
  /\bhyperliquid\b/i,
  /\bjelly\b/i,
  /\bpopcat\b/i,
];

const failures = [];
for (const relativePath of markdownFiles) {
  const absolutePath = resolve(repoRoot, relativePath);
  const source = readFileSync(absolutePath, "utf8");

  for (const term of prohibitedTerms) {
    if (term.test(source)) {
      failures.push(`${relativePath}: prohibited legacy term ${term}`);
    }
  }

  for (const match of source.matchAll(/!?\[[^\]]*\]\(([^)\s]+)(?:\s+[^)]*)?\)/g)) {
    const target = match[1].replace(/^<|>$/g, "");
    if (
      target.startsWith("#") ||
      target.startsWith("mailto:") ||
      /^[a-z][a-z0-9+.-]*:/i.test(target)
    ) {
      continue;
    }

    const localTarget = target.split(/[?#]/, 1)[0];
    if (!localTarget) continue;
    const resolvedTarget = resolve(dirname(absolutePath), localTarget);
    if (!resolvedTarget.startsWith(`${repoRoot}/`) || !existsSync(resolvedTarget)) {
      failures.push(`${relativePath}: broken local link ${target}`);
      continue;
    }
    if (statSync(resolvedTarget).isDirectory()) continue;
  }
}

if (failures.length > 0) {
  console.error("Markdown integrity check failed:");
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(`Validated ${markdownFiles.length} Markdown files: links and public naming are clean.`);
