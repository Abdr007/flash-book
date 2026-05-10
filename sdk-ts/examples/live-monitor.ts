// Live monitor — demonstrates every SDK consumer pattern in one script:
//
//   • Account fetchers: fetchMarket, fetchOrderBuffer, fetchTraderState,
//     fetchInsuranceFund, fetchFlpExposure
//   • Event subscription: subscribeToProgramEvents
//   • Risk preview: previewPortfolioRisk on a fetched portfolio
//   • Error classification: errorFamily / errorName
//
// Pure dry-run by default — prints what it WOULD do given configured RPC
// + program address. Set FLASH_BOOK_LIVE=1 to actually subscribe.
//
// Run: bun run examples/live-monitor.ts [base58_market_pda] [base58_trader]

import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import {
  FlashBookClient,
  fetchFlpExposure,
  fetchInsuranceFund,
  fetchMarket,
  fetchPosition,
  fetchTraderState,
  previewPortfolioRisk,
  subscribeToProgramEvents,
  type MarketAccount,
  type PositionAccount,
} from '../src/index.ts';

const RPC = process.env.FLASH_BOOK_RPC ?? 'https://api.devnet.solana.com';
const LIVE = process.env.FLASH_BOOK_LIVE === '1';

const connection = new Connection(RPC, 'confirmed');
const wallet = new Wallet(Keypair.generate());
const client = new FlashBookClient(connection, wallet);

function header(label: string): void {
  console.log(`\n${'━'.repeat(60)}\n  ${label}\n${'━'.repeat(60)}`);
}

function field(k: string, v: string | number | boolean): void {
  console.log(`  ${k.padEnd(28)} ${v}`);
}

const args = Bun.argv.slice(2);
const marketArg = args[0];
const traderArg = args[1];

header('Flash Book live monitor');
field('RPC', RPC);
field('Live mode', LIVE ? 'YES (will subscribe)' : 'NO (dry-run)');
field('Program', client.programId.toBase58());

// ─── Snapshot phase ───────────────────────────────────────────────────

if (LIVE) {
  header('Snapshot');

  // Insurance fund + FLP exposure are global PDAs.
  const fund = await fetchInsuranceFund(client, client.insuranceFund().address);
  if (fund) {
    field('Insurance balance', fund.balanceQuoteLots.toString());
    field('Insurance contribs', fund.totalContributions.toString());
    field('Insurance payouts', fund.totalPayouts.toString());
    field('Pause threshold', fund.pauseThresholdQuoteLots.toString());
  } else {
    field('Insurance fund', 'NOT INITIALIZED');
  }

  const flp = await fetchFlpExposure(client, client.flpExposure().address);
  if (flp) {
    field('FLP capital', flp.totalCapitalQuoteLots.toString());
    field('FLP realized PnL', flp.realizedPnl.toString());
    field('FLP markets count', flp.marketsCount);
  } else {
    field('FLP exposure', 'NOT INITIALIZED');
  }

  // Per-market state if a market PDA was provided.
  if (marketArg) {
    let marketPda: PublicKey;
    try {
      marketPda = new PublicKey(marketArg);
    } catch {
      console.error(`  Bad market arg: ${marketArg}`);
      process.exit(1);
    }
    const market = await fetchMarket(client, marketPda);
    if (market) {
      header(`Market ${marketPda.toBase58()}`);
      field('Authority', market.authority.toBase58());
      field('Status', market.status);
      field('Oracle', market.oraclePriceTicks.toString());
      field('Mark', market.markPriceTicks.toString());
      const oracleNum = market.oraclePriceTicks.toNumber();
      const markNum = market.markPriceTicks.toNumber();
      const driftBps = oracleNum > 0
        ? Math.round(((markNum - oracleNum) / oracleNum) * 10_000)
        : 0;
      field('Mark-oracle drift', `${driftBps} bps`);
      field('Current batch', market.currentBatch.toString());
      field('OI long', market.oiLongLots.toString());
      field('OI short', market.oiShortLots.toString());
      field('Total fees', market.totalFeesCollected.toString());
      field('Total liquidations', market.totalLiquidations.toString());

      // (v3 hypertree-backed orderbook depth: subscribe to OrderPlacedV2Event
      // / OrderCancelledV2Event for live state, or call view_book_depth_v2
      // for top-N levels per side.)

      // Risk preview if a trader was provided.
      if (traderArg) {
        let traderPk: PublicKey;
        try {
          traderPk = new PublicKey(traderArg);
        } catch {
          console.error(`  Bad trader arg: ${traderArg}`);
          process.exit(1);
        }
        header(`Trader ${traderPk.toBase58()}`);
        const traderState = await fetchTraderState(client, client.traderState(traderPk).address);
        if (traderState) {
          field('Collateral', traderState.collateralQuoteLots.toString());
          field('Open positions', traderState.openPositions);
          field('Realized PnL', traderState.realizedPnlQuoteLots.toString());
          field('Toxicity score', `${traderState.toxicityScoreBps} bps`);
        }
        const position = await fetchPosition(client, client.position(marketPda, traderPk).address);
        if (position && !position.sizeLots.isZero()) {
          field('Position side', position.side === 0 ? 'LONG' : 'SHORT');
          field('Position size', position.sizeLots.toString());
          field('Entry price', position.entryPriceTicks.toString());
          field('Realized PnL', position.realizedPnlQuoteLots.toString());

          // Risk preview.
          const markets = new Map<string, MarketAccount>([[marketPda.toBase58(), market]]);
          const positions: PositionAccount[] = [position];
          const collateralNum = traderState
            ? traderState.collateralQuoteLots.toNumber()
            : 0;
          const preview = previewPortfolioRisk(positions, markets, collateralNum);
          header('Risk preview (advisory)');
          field('Equity', preview.equity);
          field('Required margin', preview.required);
          field('Health ratio', preview.healthRatio.toFixed(3));
          field('Worst scenario', preview.worstScenario);
          field('Liquidatable?', preview.isHealthy ? 'NO' : 'YES (would be force-closed)');
        } else {
          field('Position', 'NONE / EMPTY');
        }
      }
    } else {
      console.log(`  Market ${marketPda.toBase58()} not found`);
    }
  }

  // ─── Subscription phase ────────────────────────────────────────────

  header('Subscribing to program events…');
  console.log('  (Ctrl-C to exit)');
  const sub = subscribeToProgramEvents(connection, (event, slot, sig) => {
    const ts = new Date().toISOString();
    switch (event.name) {
      case 'BatchClearedEvent':
        console.log(
          `  [${ts}] BatchCleared slot=${slot} ` +
            `batch=${event.data.batchNum.toString()} ` +
            `price=${event.data.clearingPrice.toString()} ` +
            `fills=${event.data.fillCount} sig=${sig.slice(0, 8)}…`,
        );
        break;
      case 'LiquidationInjectedEvent':
        console.log(
          `  [${ts}] 🔥 LiquidationInjected trader=${event.data.trader.toBase58().slice(0, 8)}… ` +
            `size=${event.data.sizeLots.toString()} scenario=${event.data.worstScenarioIdx}`,
        );
        break;
      case 'FillAppliedEvent':
        console.log(
          `  [${ts}] FillApplied size=${event.data.sizeLots.toString()} ` +
            `price=${event.data.priceTicks.toString()} ` +
            `taker=${event.data.taker.toBase58().slice(0, 8)}…`,
        );
        break;
      case 'MarketStatusChangedEvent':
        console.log(
          `  [${ts}] ⚠️  MarketStatusChanged ${event.data.previousStatus} → ${event.data.newStatus}`,
        );
        break;
      default:
        console.log(`  [${ts}] ${event.name}`);
    }
  });

  process.on('SIGINT', () => {
    console.log('\n  Unsubscribing…');
    sub.unsubscribe();
    process.exit(0);
  });

  // Keep the process alive.
  await new Promise(() => {});
} else {
  header('Dry-run mode');
  console.log('  Set FLASH_BOOK_LIVE=1 to fetch state and subscribe to events.');
  console.log('  Optional positional args:');
  console.log('    [1] market PDA (base58) — to print market + buffer state');
  console.log('    [2] trader pubkey       — to print trader state, position, risk');
  console.log('');
  console.log('  Example:');
  console.log('    FLASH_BOOK_LIVE=1 \\');
  console.log('      bun run examples/live-monitor.ts \\');
  console.log('      <market_pda> <trader_pubkey>');
}
