// Off-chain keeper bots for Flash Book V3.
//
// Four keepers ship in this module — all V3-specific (V2 has its own
// liquidator infrastructure separate from this bot suite):
//
//   1. LiquidationKeeper — scans monitored (market, trader) pairs,
//      previews portfolio risk against the stress lattice, fires
//      liquidate_position when a trader breaches maintenance margin.
//
//   2. FundingKeeper — periodically calls settle_funding on a fixed
//      list of (market, trader) pairs. Settlement is permissionless
//      and idempotent; the bot's only job is paying the tx fee for
//      stale positions that haven't naturally turned over.
//
//   3. InvariantMonitor — periodically calls verify_market_invariants.
//      The on-chain ix returns Err(OpenInterestImbalance) on breach;
//      the keeper logs + alerts via a callback so an operator can
//      explicitly flip the market to Paused via authority.
//
//   4. AtaCleanupKeeper — closes empty trader quote ATAs after a
//      configurable idle period (rent reclamation). Only fires when
//      the ATA balance is zero AND the trader has no active positions.
//
// All four share a `Keeper` base class with start/stop/stats. Discovery
// is operator-supplied (not auto-scanned via getProgramAccounts) — this
// keeps RPC load predictable. Production deployments wire the (market,
// trader) lists from a separate indexer.

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  type TransactionInstruction,
} from '@solana/web3.js';
import { FlashBookClient } from '../../sdk-ts/src/client.ts';
import {
  fetchMarket,
  fetchPosition,
  fetchTraderState,
  type MarketAccount as MarketAcc,
  type PositionAccount as PositionAcc,
} from '../../sdk-ts/src/accounts.ts';
import { previewPortfolioRisk } from '../../sdk-ts/src/risk-preview.ts';

// ─── Shared base ──────────────────────────────────────────────────────

export interface KeeperBaseConfig {
  /// Refresh interval in ms.
  refreshMs: number;
  /// Tx-fee payer for the keeper's instructions.
  signer: Keypair;
  /// If true, compute decisions + log but never send tx.
  dryRun?: boolean;
}

export interface KeeperStats {
  startedAt: number;
  iterationsCompleted: number;
  actionsTaken: number;
  txErrors: number;
  lastError?: string | undefined;
}

export abstract class Keeper {
  protected readonly stats: KeeperStats;
  private timer: ReturnType<typeof setInterval> | null = null;
  private busy = false;

  constructor(
    protected readonly client: FlashBookClient,
    protected readonly connection: Connection,
    protected readonly base: KeeperBaseConfig,
  ) {
    this.stats = {
      startedAt: Date.now(),
      iterationsCompleted: 0,
      actionsTaken: 0,
      txErrors: 0,
    };
  }

  abstract readonly name: string;

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => {
      if (this.busy) return;
      void this.tick();
    }, this.base.refreshMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  getStats(): Readonly<KeeperStats> {
    return { ...this.stats };
  }

  /// Single iteration. Public for explicit-call testability.
  async tick(): Promise<void> {
    this.busy = true;
    try {
      await this.iterate();
      this.stats.lastError = undefined;
    } catch (e) {
      this.stats.txErrors += 1;
      this.stats.lastError = e instanceof Error ? e.message : String(e);
    } finally {
      this.stats.iterationsCompleted += 1;
      this.busy = false;
    }
  }

  protected abstract iterate(): Promise<void>;

  protected async sendIxs(ixs: TransactionInstruction[]): Promise<string | null> {
    if (ixs.length === 0) return null;
    if (this.base.dryRun) return null;
    const tx = new Transaction().add(...ixs);
    tx.recentBlockhash = (await this.connection.getLatestBlockhash()).blockhash;
    tx.feePayer = this.base.signer.publicKey;
    tx.sign(this.base.signer);
    const sig = await this.connection.sendRawTransaction(tx.serialize());
    await this.connection.confirmTransaction(sig, 'confirmed');
    return sig;
  }
}

// ─── Liquidation keeper ───────────────────────────────────────────────

export interface LiquidationKeeperConfig extends KeeperBaseConfig {
  /// (market, trader) pairs to monitor.
  watchlist: ReadonlyArray<{ market: PublicKey; trader: PublicKey }>;
  /// Health-ratio threshold — liquidate when `equity / required ≤` this.
  /// Default 1.0 (just-underwater). Operators may set <1.0 to wait for
  /// deeper underwater states before paying the liquidation tx.
  healthThreshold?: number;
}

export class LiquidationKeeper extends Keeper {
  readonly name = 'liquidation-keeper';
  private readonly threshold: number;

  constructor(
    client: FlashBookClient,
    connection: Connection,
    private readonly cfg: LiquidationKeeperConfig,
  ) {
    super(client, connection, cfg);
    this.threshold = cfg.healthThreshold ?? 1.0;
  }

  protected async iterate(): Promise<void> {
    for (const { market, trader } of this.cfg.watchlist) {
      const decision = await this.evaluate(market, trader);
      if (!decision.shouldLiquidate) continue;
      const ix = await this.client.liquidatePositionIx({
        caller: this.base.signer.publicKey,
        market,
        trader,
      });
      const sig = await this.sendIxs([ix]);
      if (sig) {
        this.stats.actionsTaken += 1;
      }
    }
  }

  /// Pure decision step — exposed for testing.
  async evaluate(market: PublicKey, trader: PublicKey): Promise<{
    shouldLiquidate: boolean;
    healthRatio: number;
    reason: string;
  }> {
    const [marketAcc, positionAcc, traderState] = await Promise.all([
      fetchMarket(this.client, market),
      fetchPosition(this.client, this.client.position(market, trader).address),
      fetchTraderState(this.client, this.client.traderState(trader).address),
    ]);
    if (!marketAcc || !positionAcc || !traderState) {
      return { shouldLiquidate: false, healthRatio: Infinity, reason: 'account missing' };
    }
    if (positionAcc.sizeLots.isZero()) {
      return { shouldLiquidate: false, healthRatio: Infinity, reason: 'empty position' };
    }
    const markets = new Map([[market.toBase58(), marketAcc]]);
    const preview = previewPortfolioRisk(
      [positionAcc],
      markets,
      Number(traderState.collateralQuoteLots.toString()),
    );
    const healthy = preview.healthRatio > this.threshold;
    return {
      shouldLiquidate: !healthy,
      healthRatio: preview.healthRatio,
      reason: healthy ? 'healthy' : `health=${preview.healthRatio.toFixed(3)} ≤ ${this.threshold}`,
    };
  }
}

// ─── Funding keeper ───────────────────────────────────────────────────

export interface FundingKeeperConfig extends KeeperBaseConfig {
  /// (market, trader) pairs to sweep.
  watchlist: ReadonlyArray<{ market: PublicKey; trader: PublicKey }>;
  /// Skip a position if estimated owed funding is below this absolute
  /// threshold in quote lots (avoid wasting tx fees on micro-deltas).
  /// Default 0 (sweep everything).
  minOwedQuoteLots?: bigint;
}

export class FundingKeeper extends Keeper {
  readonly name = 'funding-keeper';

  constructor(
    client: FlashBookClient,
    connection: Connection,
    private readonly cfg: FundingKeeperConfig,
  ) {
    super(client, connection, cfg);
  }

  protected async iterate(): Promise<void> {
    const minOwed = this.cfg.minOwedQuoteLots ?? 0n;
    for (const { market, trader } of this.cfg.watchlist) {
      const decision = await this.evaluate(market, trader, minOwed);
      if (!decision.shouldSweep) continue;
      const ix = await this.client.settleFundingIx({
        caller: this.base.signer.publicKey,
        market,
        trader,
      });
      const sig = await this.sendIxs([ix]);
      if (sig) this.stats.actionsTaken += 1;
    }
  }

  /// Decide whether to sweep based on the current funding-index delta.
  /// Pure read of on-chain state, no tx.
  async evaluate(
    market: PublicKey,
    trader: PublicKey,
    minOwedQuoteLots: bigint,
  ): Promise<{ shouldSweep: boolean; estimatedOwedQuoteLots: bigint; reason: string }> {
    const [m, p] = await Promise.all([
      fetchMarket(this.client, market),
      fetchPosition(this.client, this.client.position(market, trader).address),
    ]);
    if (!m || !p) return { shouldSweep: false, estimatedOwedQuoteLots: 0n, reason: 'account missing' };
    if (p.sizeLots.isZero()) return { shouldSweep: false, estimatedOwedQuoteLots: 0n, reason: 'empty position' };
    const owed = estimateFundingOwed(p, m);
    const owedAbs = owed < 0n ? -owed : owed;
    if (owedAbs < minOwedQuoteLots) {
      return {
        shouldSweep: false,
        estimatedOwedQuoteLots: owed,
        reason: `|owed|=${owedAbs} < min=${minOwedQuoteLots}`,
      };
    }
    return { shouldSweep: true, estimatedOwedQuoteLots: owed, reason: `|owed|=${owedAbs}` };
  }
}

/// Estimate funding owed by a position at the market's current cum index.
/// Mirrors the on-chain `funding_owed` math:
///   owed = ±notional × (cum_now − cum_at_entry) >> 64
/// Sign: + for long, − for short. Exported for tests.
export function estimateFundingOwed(position: PositionAcc, market: MarketAcc): bigint {
  const size = BigInt(position.sizeLots.toString());
  const entry = BigInt(position.entryPriceTicks.toString());
  const tick = BigInt(market.params.tickSize.toString());
  const notional = size * entry * tick;
  const cumNow = BigInt(market.cumFundingIndex.toString());
  const cumAtEntry = BigInt(position.cumFundingIndexAtEntry.toString());
  const delta = cumNow - cumAtEntry;
  const isLong = position.side === 0;
  const sign = isLong ? 1n : -1n;
  // Q64.64 → linear: (notional × delta) >> 64.
  const product = notional * delta;
  const scaled = product >> 64n;
  return sign * scaled;
}

// ─── Invariant monitor ────────────────────────────────────────────────

export interface InvariantMonitorConfig extends KeeperBaseConfig {
  /// Markets to verify each iteration.
  markets: ReadonlyArray<PublicKey>;
  /// Called when a verify_market_invariants tx fails (= invariant breach
  /// or an off-chain network error). Operator wires this to PagerDuty,
  /// Slack, or whatever they monitor.
  onAlert?: (info: { market: PublicKey; error: string }) => void;
}

export class InvariantMonitor extends Keeper {
  readonly name = 'invariant-monitor';

  constructor(
    client: FlashBookClient,
    connection: Connection,
    private readonly cfg: InvariantMonitorConfig,
  ) {
    super(client, connection, cfg);
  }

  protected async iterate(): Promise<void> {
    for (const market of this.cfg.markets) {
      const ix = await this.client.verifyMarketInvariantsIx({
        caller: this.base.signer.publicKey,
        market,
      });
      try {
        const sig = await this.sendIxs([ix]);
        if (sig) this.stats.actionsTaken += 1;
      } catch (e) {
        const errMsg = e instanceof Error ? e.message : String(e);
        // verify_market_invariants returning Err is the breach signal.
        // Surface to the operator via the alert callback; do NOT let it
        // bubble up and crash the tick (other markets still need checks).
        if (this.cfg.onAlert) {
          this.cfg.onAlert({ market, error: errMsg });
        }
      }
    }
  }
}

// ─── ATA cleanup keeper ───────────────────────────────────────────────

export interface AtaCleanupKeeperConfig extends KeeperBaseConfig {
  /// Traders whose ATAs should be considered for cleanup.
  watchlist: ReadonlyArray<{
    trader: PublicKey;
    quoteMint: PublicKey;
    /// Where to send the rent rebate. Defaults to trader.
    rentDestination?: PublicKey;
  }>;
}

export class AtaCleanupKeeper extends Keeper {
  readonly name = 'ata-cleanup-keeper';

  constructor(
    client: FlashBookClient,
    connection: Connection,
    private readonly cfg: AtaCleanupKeeperConfig,
  ) {
    super(client, connection, cfg);
  }

  protected async iterate(): Promise<void> {
    for (const entry of this.cfg.watchlist) {
      const decision = await this.evaluate(entry.trader);
      if (!decision.shouldClose) continue;
      const ix = await this.client.closeTraderAtaIx({
        trader: entry.trader,
        quoteMint: entry.quoteMint,
        rentDestination: entry.rentDestination ?? entry.trader,
      });
      const sig = await this.sendIxs([ix]);
      if (sig) this.stats.actionsTaken += 1;
    }
  }

  async evaluate(trader: PublicKey): Promise<{ shouldClose: boolean; reason: string }> {
    const traderState = await fetchTraderState(
      this.client,
      this.client.traderState(trader).address,
    );
    if (!traderState) return { shouldClose: false, reason: 'no trader_state' };
    if (traderState.openPositions > 0) {
      return { shouldClose: false, reason: `${traderState.openPositions} open positions` };
    }
    if (!traderState.collateralQuoteLots.isZero()) {
      return {
        shouldClose: false,
        reason: `collateral ${traderState.collateralQuoteLots.toString()} > 0`,
      };
    }
    return { shouldClose: true, reason: 'no positions, no collateral' };
  }
}

// ─── ADL keeper ───────────────────────────────────────────────────────

export interface AdlKeeperConfig extends KeeperBaseConfig {
  /// Markets to monitor for ADL conditions.
  markets: ReadonlyArray<PublicKey>;
  /// Off-chain candidate set: traders the keeper considers as either
  /// underwater OR potential counter-traders. Production deployments
  /// hydrate this from a subgraph indexing FillAppliedEvent +
  /// CollateralDepositedEvent; the bot does not auto-discover via
  /// getProgramAccounts to keep RPC load predictable.
  candidates: ReadonlyArray<PublicKey>;
  /// Minimum insurance-fund-balance / pause-threshold ratio below which
  /// we even consider ADL. 0.0–1.0; 0.5 means "trigger when insurance
  /// is below 50% of pause threshold." Default 1.0 (chain enforces
  /// `< pause_threshold` strictly; this is a bot-side guard against
  /// firing too eagerly when the gap is narrow).
  insuranceRatioFloor?: number;
}

interface AdlScored {
  trader: PublicKey;
  position: PositionAcc;
  unrealizedPnl: number;
  leverage: number;
  rank: number;
}

/// Monitors a set of markets; when insurance falls below the chain
/// trigger AND an underwater position exists, ranks profitable counter-
/// traders by (unrealized_pnl × leverage) and submits the top-ranked
/// candidate to `auto_deleverage`. Eligibility (counter profitable at
/// the bankruptcy price; underwater actually sick; insurance below
/// pause threshold) is re-checked on chain — invalid ranking just
/// rejects so the bot retries with the next candidate.
export class AdlKeeper extends Keeper {
  readonly name = 'adl-keeper';

  constructor(
    client: FlashBookClient,
    connection: Connection,
    private readonly cfg: AdlKeeperConfig,
  ) {
    super(client, connection, cfg);
  }

  protected async iterate(): Promise<void> {
    for (const market of this.cfg.markets) {
      await this.iterateMarket(market);
    }
  }

  private async iterateMarket(market: PublicKey): Promise<void> {
    // 1. Insurance-fund trigger gate (off-chain pre-check; chain
    //    re-checks `balance < pause_threshold` strictly).
    const fund = await this.fetchInsuranceFund();
    if (!fund) return;
    const ratioFloor = this.cfg.insuranceRatioFloor ?? 1.0;
    const ratio = Number(fund.balanceQuoteLots.toString())
      / Math.max(Number(fund.pauseThresholdQuoteLots.toString()), 1);
    if (ratio >= ratioFloor) return; // no ADL warranted

    // 2. Snapshot every candidate's position on this market.
    const marketAcc = await fetchMarket(this.client, market);
    if (!marketAcc) return;

    const longs: AdlScored[] = [];
    const shorts: AdlScored[] = [];
    let underwater: { trader: PublicKey; position: PositionAcc } | null = null;
    for (const trader of this.cfg.candidates) {
      const pos = await fetchPosition(
        this.client,
        this.client.position(market, trader).address,
      );
      if (!pos || pos.sizeLots.isZero()) continue;
      const ts = await fetchTraderState(
        this.client,
        this.client.traderState(trader).address,
      );
      if (!ts) continue;

      // Health check (single-position approx, mirroring chain's stress
      // lattice for a single-market view).
      const markets = new Map([[market.toBase58(), marketAcc]]);
      const preview = previewPortfolioRisk(
        [pos],
        markets,
        Number(ts.collateralQuoteLots.toString()),
      );
      if (preview.healthRatio <= 1.0) {
        // First sick wins for this iteration.
        if (!underwater) underwater = { trader, position: pos };
        continue;
      }

      // Score for ranking: (unrealized_pnl × leverage). Larger = higher
      // priority for ADL (HL-style ranking).
      const tickSize = Number(marketAcc.params.tickSize.toString());
      const mark = Number(marketAcc.markPriceTicks.toString());
      const entry = Number(pos.entryPriceTicks.toString());
      const size = Number(pos.sizeLots.toString());
      const sign = pos.side === 0 ? 1 : -1;
      const upnl = sign * size * (mark - entry) * tickSize;
      if (upnl <= 0) continue; // only profitable counter-traders are eligible
      const collateral = Number(ts.collateralQuoteLots.toString());
      const notional = size * mark * tickSize;
      const leverage = collateral > 0 ? notional / collateral : 0;
      const scored: AdlScored = { trader, position: pos, unrealizedPnl: upnl, leverage, rank: upnl * leverage };
      (pos.side === 0 ? longs : shorts).push(scored);
    }

    if (!underwater) return;
    // Counter-side is opposite of underwater.
    const candidates = underwater.position.side === 0 ? shorts : longs;
    candidates.sort((a, b) => b.rank - a.rank);
    const top = candidates[0];
    if (!top) return; // no profitable counter — only normal liq path remains

    const closeSize = bigMin(
      BigInt(underwater.position.sizeLots.toString()),
      BigInt(top.position.sizeLots.toString()),
    );
    if (closeSize === 0n) return;

    const ix = await this.client.autoDeleverageIx({
      caller: this.base.signer.publicKey,
      market,
      underwaterTrader: underwater.trader,
      counterTrader: top.trader,
      closeSizeLots: closeSize,
    });
    const sig = await this.sendIxs([ix]);
    if (sig) this.stats.actionsTaken += 1;
  }

  private async fetchInsuranceFund(): Promise<
    { balanceQuoteLots: { toString(): string }; pauseThresholdQuoteLots: { toString(): string } } | null
  > {
    const { fetchInsuranceFund } = await import('../../sdk-ts/src/accounts.ts');
    return fetchInsuranceFund(this.client, this.client.insuranceFund().address);
  }
}

function bigMin(a: bigint, b: bigint): bigint {
  return a < b ? a : b;
}
