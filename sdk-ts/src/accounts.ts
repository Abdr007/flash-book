// Typed account fetchers and decoders.
//
// Anchor's `Program<T>` carries the IDL but the JSON-IDL form gives us
// only a loosely-typed account namespace. The wrappers here expose:
//   - typed fetchers (returning the canonical TS shape per account type)
//   - typed multi-fetch
//   - manual decoder (for raw account data buffers from getProgramAccounts)
// All address resolution still goes through the PDA helpers.

import { BorshAccountsCoder } from '@coral-xyz/anchor';
import type { PublicKey } from '@solana/web3.js';
import type BN from 'bn.js';
import type { FlashBookClient } from './client.ts';
import { IDL } from './client.ts';

// ─── On-chain account shapes (mirror programs/flash-book/src/state.rs) ──

export interface MarketAccount {
  authority: PublicKey;
  flpPool: PublicKey;
  baseMint: PublicKey;
  quoteMint: PublicKey;
  baseVault: PublicKey;
  quoteVault: PublicKey;
  oracleAccount: PublicKey;
  insuranceFund: PublicKey;
  bump: number;
  status: number;
  currentBatch: BN;
  lastBatchMs: BN;
  oraclePriceTicks: BN;
  oracleConfidence: BN;
  markPriceTicks: BN;
  cumFundingIndex: BN;
  lastFundingRateBpsPerSec: BN;
  vpin: VpinState;
  oiLongLots: BN;
  oiShortLots: BN;
  recentClearingPrices: BN[];
  recentClearingCount: number;
  totalFeesCollected: BN;
  totalToxicityTaxCollected: BN;
  totalLiquidations: BN;
  params: MarketParamsAccount;
}

export interface VpinState {
  buyPending: BN;
  sellPending: BN;
  bucketsObserved: BN;
  valueQ32_32: BN;
}

export interface MarketParamsAccount {
  tickSize: BN;
  baseLotSize: BN;
  quoteLotSize: BN;
  minBaseLots: BN;
  takerFeeBps: number;
  makerRebateBps: number;
  toxicityTaxMaxBps: number;
  liqPenaltyBps: number;
  maintenanceMarginRatioBps: number;
  initialMarginRatioBps: number;
  maxLeverage: number;
  fundingRateMaxBpsPerSec: number;
  fundingRateKBps: number;
  oracleBandBps: number;
  flpSpreadBaseBps: number;
  flpSpreadAlphaBps: number;
  flpSpreadBetaBps: number;
  flpSpreadGammaBps: number;
  flpSpreadKappaBps: number;
  flpSpreadDeltaBps: number;
  flpInventoryLambdaBps: number;
  flpDepthFloorLots: BN;
  flpMaxGrowthPerBatchBps: number;
  flpQuoteLevels: number;
  vpinBucketSizeLots: BN;
  vpinEmaWindow: number;
  twapWindow: number;
  batchIntervalMs: number;
}

export interface OrderBufferAccount {
  market: PublicKey;
  bump: number;
  head: number;
  seqCounter: BN;
  slots: OrderSlot[];
}

export interface OrderSlot {
  valid: number;
  side: number;
  orderType: number;
  postOnly: number;
  seq: BN;
  id: BN;
  trader: PublicKey;
  sizeLots: BN;
  limitTicks: BN;
}

export interface CommitBufferAccount {
  market: PublicKey;
  bump: number;
  head: number;
  commits: CommitRow[];
}

export interface CommitRow {
  hash: number[];
  trader: PublicKey;
  bond: BN;
  committedAtBatch: BN;
  expireAtBatch: BN;
  valid: number;
}

export interface InsuranceFundAccount {
  authority: PublicKey;
  bump: number;
  balanceQuoteLots: BN;
  feeContributionBps: number;
  toxicityTaxContributionBps: number;
  liqPenaltyContributionBps: number;
  pauseThresholdQuoteLots: BN;
  totalContributions: BN;
  totalPayouts: BN;
}

export interface FlpExposureAccount {
  authority: PublicKey;
  bump: number;
  totalCapitalQuoteLots: BN;
  realizedPnl: BN;
  marketsCount: number;
  perMarket: FlpMarketExposure[];
}

export interface FlpMarketExposure {
  market: PublicKey;
  side: number;
  sizeLots: BN;
  entryPriceTicks: BN;
}

export interface PositionAccount {
  trader: PublicKey;
  market: PublicKey;
  bump: number;
  side: number;
  sizeLots: BN;
  entryPriceTicks: BN;
  collateralQuoteLots: BN;
  cumFundingIndexAtEntry: BN;
  realizedPnlQuoteLots: BN;
  fundingPaidQuoteLots: BN;
  lastSettlementBatch: BN;
}

export interface TraderStateAccount {
  trader: PublicKey;
  bump: number;
  collateralQuoteLots: BN;
  realizedPnlQuoteLots: BN;
  openPositions: number;
  toxicityScoreBps: number;
  ordersThisBatch: number;
  lastBatchSeen: BN;
}

// ─── Fetch helpers ─────────────────────────────────────────────────────

type AccountNamespace = Record<string, {
  fetch: (address: PublicKey) => Promise<unknown>;
  fetchNullable: (address: PublicKey) => Promise<unknown | null>;
  fetchMultiple: (addresses: PublicKey[]) => Promise<Array<unknown | null>>;
}>;

function ns(client: FlashBookClient): AccountNamespace {
  return client.program.account as unknown as AccountNamespace;
}

export async function fetchMarket(client: FlashBookClient, address: PublicKey): Promise<MarketAccount | null> {
  return (await ns(client).marketAccount.fetchNullable(address)) as MarketAccount | null;
}

export async function fetchOrderBuffer(
  client: FlashBookClient,
  address: PublicKey,
): Promise<OrderBufferAccount | null> {
  return (await ns(client).orderBufferAccount.fetchNullable(address)) as OrderBufferAccount | null;
}

export async function fetchCommitBuffer(
  client: FlashBookClient,
  address: PublicKey,
): Promise<CommitBufferAccount | null> {
  return (await ns(client).commitBufferAccount.fetchNullable(address)) as CommitBufferAccount | null;
}

export async function fetchInsuranceFund(
  client: FlashBookClient,
  address: PublicKey,
): Promise<InsuranceFundAccount | null> {
  return (await ns(client).insuranceFundAccount.fetchNullable(address)) as InsuranceFundAccount | null;
}

export async function fetchFlpExposure(
  client: FlashBookClient,
  address: PublicKey,
): Promise<FlpExposureAccount | null> {
  return (await ns(client).flpExposureAccount.fetchNullable(address)) as FlpExposureAccount | null;
}

export async function fetchTraderState(
  client: FlashBookClient,
  address: PublicKey,
): Promise<TraderStateAccount | null> {
  return (await ns(client).traderStateAccount.fetchNullable(address)) as TraderStateAccount | null;
}

export async function fetchPosition(
  client: FlashBookClient,
  address: PublicKey,
): Promise<PositionAccount | null> {
  return (await ns(client).positionAccount.fetchNullable(address)) as PositionAccount | null;
}

// ─── Manual decoder ────────────────────────────────────────────────────

/**
 * Decode an account data buffer (from `getProgramAccounts`, an RPC
 * subscription, or a snapshot) into the typed shape. The discriminator
 * is the first 8 bytes; pass the full account data including it.
 */
export function decodeAccount<T>(
  accountName:
    | 'marketAccount'
    | 'orderBufferAccount'
    | 'commitBufferAccount'
    | 'insuranceFundAccount'
    | 'flpExposureAccount'
    | 'traderStateAccount'
    | 'positionAccount',
  data: Buffer,
): T {
  const coder = new BorshAccountsCoder(IDL);
  return coder.decode(accountName, data) as T;
}
