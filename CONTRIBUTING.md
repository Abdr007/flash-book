# Contributing

Thanks for considering a contribution.

## Ground rules

- **Numerical safety first.** Every new financial calculation goes through
  `safeNumber()` and explicit `Number.isFinite()` guards. Use the existing
  `src/math.ts` helpers — don't roll your own.
- **No floating-point drift in the matcher.** The matcher operates on
  comparable prices/sizes; if you introduce arithmetic that depends on
  operand order, prove it doesn't change clearing outcomes.
- **Tests are required.** Every PR ships at least one test demonstrating
  the change. Property-style tests (random-flow invariant assertions) are
  preferred for matcher / engine changes.
- **Strict TypeScript.** This repo uses `strict`, `noUncheckedIndexedAccess`,
  and `exactOptionalPropertyTypes`. Don't paper over with `any` or `!`.
  Index access returns `T | undefined`; handle it.
- **Determinism.** All randomness comes through the seeded `Prng`. No
  `Math.random()` in source files. Tests may use it for fuzz inputs but
  must assert invariants, not specific values.

## Development

```bash
bun install
bun test
bunx tsc --noEmit
bun run examples/synthetic-flow.ts
```

## Commit style

```
<scope>: <imperative summary>

<body explaining why the change is needed; what changed at a high level;
any follow-ups or known limitations>
```

Scopes: `matcher`, `flp-quoter`, `funding`, `risk`, `liquidation`,
`insurance`, `commit-reveal`, `engine`, `tests`, `docs`, `chore`.

## Reporting safety issues

Do not open public issues for safety vulnerabilities. Email the
maintainers or use a private security advisory.
