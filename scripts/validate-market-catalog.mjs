import { readFileSync } from "node:fs";

const path = process.argv[2] ?? "config/mainnet-markets.json";
const catalog = JSON.parse(readFileSync(path, "utf8"));
const base58Alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const feedId = /^[0-9a-f]{64}$/;

function fail(message) {
  console.error(`market catalog validation failed: ${message}`);
  process.exit(1);
}

function isPublicKey(value) {
  if (typeof value !== "string" || value.length === 0) return false;
  let decoded = 0n;
  for (const character of value) {
    const index = base58Alphabet.indexOf(character);
    if (index < 0) return false;
    decoded = decoded * 58n + BigInt(index);
  }
  const significantBytes = decoded === 0n ? 0 : Math.ceil(decoded.toString(16).length / 2);
  const leadingZeroBytes = value.match(/^1*/)[0].length;
  return significantBytes + leadingZeroBytes === 32;
}

if (catalog.schema !== "clober.mainnet-market-catalog" || catalog.schemaVersion !== 1) {
  fail("unsupported schema");
}
if (catalog.network !== "mainnet-beta") fail("network must be mainnet-beta");
if (!isPublicKey(catalog.quoteMint)) fail("quoteMint is not a 32-byte base58 public key");
if (catalog.quoteSymbol !== "USDC") fail("quote collateral must be native USDC");

const oracle = catalog.oracle;
if (!oracle || oracle.provider !== "pyth") fail("Pyth must be the configured oracle");
if (!isPublicKey(oracle.receiverProgram)) fail("invalid Pyth receiver program");
if (!Number.isInteger(oracle.maxStalenessSeconds) || oracle.maxStalenessSeconds < 1 || oracle.maxStalenessSeconds > 60) {
  fail("oracle maxStalenessSeconds must be between 1 and 60");
}
if (!Number.isInteger(oracle.maxConfidenceBps) || oracle.maxConfidenceBps < 1 || oracle.maxConfidenceBps > 1_000) {
  fail("oracle maxConfidenceBps must be between 1 and 1000");
}
if (!Number.isInteger(oracle.tickDecimals) || oracle.tickDecimals < 0 || oracle.tickDecimals > 18) {
  fail("oracle tickDecimals must be between 0 and 18");
}

if (!Array.isArray(catalog.markets) || catalog.markets.length < 5 || catalog.markets.length > 7) {
  fail("catalog must contain between five and seven markets");
}

const symbols = new Set();
const mints = new Set();
const feeds = new Set();
for (const market of catalog.markets) {
  if (!/^[A-Z0-9]+-PERP$/.test(market.symbol)) fail(`invalid symbol ${market.symbol}`);
  if (symbols.has(market.symbol)) fail(`duplicate symbol ${market.symbol}`);
  if (!isPublicKey(market.baseMint)) fail(`${market.symbol} has an invalid base mint`);
  if (mints.has(market.baseMint)) fail(`${market.symbol} duplicates a base mint`);
  if (!Number.isInteger(market.baseDecimals) || market.baseDecimals < 0 || market.baseDecimals > 18) {
    fail(`${market.symbol} has invalid base decimals`);
  }
  if (!feedId.test(market.pythFeedId)) fail(`${market.symbol} has an invalid Pyth feed ID`);
  if (feeds.has(market.pythFeedId)) fail(`${market.symbol} duplicates a Pyth feed ID`);
  if (!["major", "liquid-alt"].includes(market.riskProfile)) fail(`${market.symbol} has an unknown risk profile`);
  for (const key of ["initialMarginBps", "maintenanceMarginBps", "maxLeverage", "oracleBandBps"]) {
    if (!Number.isInteger(market[key]) || market[key] <= 0) fail(`${market.symbol} has invalid ${key}`);
  }
  if (market.initialMarginBps < market.maintenanceMarginBps) fail(`${market.symbol} has initial margin below maintenance margin`);
  if (market.maxLeverage > Math.floor(10_000 / market.initialMarginBps)) fail(`${market.symbol} leverage exceeds its initial margin`);
  symbols.add(market.symbol);
  mints.add(market.baseMint);
  feeds.add(market.pythFeedId);
}

console.log(`Validated ${catalog.markets.length} mainnet launch markets with unique mints and Pyth feeds.`);
