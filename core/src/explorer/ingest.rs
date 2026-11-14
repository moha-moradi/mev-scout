//! Explorer ingest — block/receipt fetch, decode, classify, persist.
//!
//! Consumes the same `get_block_and_receipts_batch` path the scanner uses,
//! decodes logs into explorer facts, runs the classifier, and persists into
//! the explorer store. Idempotent per block (checkpoints in
//! `blocks_classified`); reorg-aware via stored block hashes; confirmation
//! lag applied before indexing.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, B256, U256};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::data::{BlockData, ReceiptData, TxData};
use crate::explorer::classify::{self, BlockContext, BlockInput, TxInput};
use crate::explorer::jit_accounting::{self, ArchiveBudget, JitAccounting, LpBlock, Quote};
use crate::explorer::pricing::{self, TokenUsd};
use crate::explorer::store::{
    BlockFactsInput, ExplorerStore, JitJournalInput, SwapRow, TransferRow, TxRow,
};
use crate::explorer::types::{MevEvent, MevKind};
use crate::progress::{JobProgress, ProgressEvent};
use crate::rpc::RpcClient;
use crate::types::ChainName;

/// Block header + txs + receipts from a single batched RPC fetch.
pub type BlockBundle = (BlockData, Vec<TxData>, Vec<ReceiptData>);

/// Per-block fetch payload for [`index_block_fetched`] (not classify [`BlockInput`]).
#[derive(Debug)]
pub struct FetchedBlockInput<'a> {
    pub block_number: u64,
    pub native_price: Option<f64>,
    pub token_prices: Option<HashMap<Address, TokenUsd>>,
    pub bundle: BlockBundle,
    /// Run-scoped classification context, shared across the whole backfill.
    pub ctx: &'a BlockContext,
}

/// Block range and concurrency for [`run_range`].
#[derive(Debug, Clone, Copy)]
pub struct RangeRequest {
    pub from_block: u64,
    pub to_block: u64,
    pub block_concurrency: usize,
}

/// JIT open-position window: positions opened more than this many
/// blocks before the indexed block are pruned.
pub const JIT_WINDOW_BLOCKS: u64 = 1_000;

/// Ingester configuration.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub chain: ChainName,
    pub chain_id: u64,
    /// Blocks of lag behind head before indexing (default 6 ≈ 12s on Polygon).
    pub confirmations: u64,
    /// Wrapped-native token from chain config (wrap-noise filter).
    pub wrapped_native: Address,
    /// Profit-token priority from chain config.
    pub profit_token_priority: Vec<Address>,
    /// Emitter-address → protocol label relabels for liquidation events whose
    /// topic0 is shared with another protocol (Aave-V3 ABI: Spark / Aave V2;
    /// Compound V2 ABI: Benqi qiTokens / Venus vTokens — plan P0.2 /).
    pub liquidation_protocol_aliases: HashMap<Address, &'static str>,
    /// Compound-V2-fork cToken → underlying ERC-20 (seizeTokens conversion
    /// before PnL). Shared by Benqi and Venus.
    pub c_token_underlyings: HashMap<Address, Address>,
    /// Chainlink aggregator → underlying asset (plan P1.1 / P1.4).
    pub chainlink_feeds: HashMap<Address, Address>,
    pub savax: Option<Address>,
    pub gmx_event_emitters: HashSet<Address>,
    /// GMX market tokens an ADL-adjacent arb must touch (P3.7 route linkage).
    pub gmx_market_tokens: HashSet<Address>,
    /// Extra tip/fee-recipient sinks merged with each block's coinbase so
    /// forwarded residuals to builders are not counted as searcher PnL.
    pub tip_sink_addresses: HashSet<Address>,
    /// Persist explorer-classify near-miss drops into `rejected_candidates`
    /// (same flag as scanner `record_rejections`).
    pub record_rejections: bool,
    /// Blocks per JSON-RPC batch POST during [`run_range`]. `1` fetches one
    /// block per POST.
    pub rpc_batch_blocks: usize,
    /// Causal-pair window (D12); defaults to the compiled constant.
    pub causal_window_txs: u64,
    pub causal_price_min_pct: f64,
    pub jit_window_blocks: u64,
    pub interest_lookback_blocks: u64,
    pub jit_pool_fee_bps: u32,
}

impl IngestConfig {
    pub fn from_chain(chain: ChainName, chain_config: &crate::config::ChainConfig) -> Self {
        let wrapped_native = chain_config.wrapped_native_token.unwrap_or(Address::ZERO);
        let priority = build_profit_priority(chain_config, wrapped_native);
        // `LiquidationFact.protocol` is a borrowed `&'static str`, so config
        // labels are interned once here. The leak is bounded by the config: a
        // handful of aliases per chain, at process start. Removing it means
        // turning `protocol` into an owned String across decode/classify.
        let intern = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        let mut liquidation_protocol_aliases = HashMap::new();
        if let Some(v2) = chain_config.aave_v2_pool {
            liquidation_protocol_aliases.insert(v2, "aave_v2");
        }
        if let Some(spark) = chain_config.spark_pool {
            liquidation_protocol_aliases.insert(spark, "spark");
        }
        if let Some(aliases) = &chain_config.liquidation_protocol_aliases {
            liquidation_protocol_aliases.extend(aliases.iter().map(|(a, l)| (*a, intern(l))));
        }
        let chainlink_feeds = chain_config.chainlink_feeds.clone().unwrap_or_default();
        let c_token_underlyings = chain_config.c_token_underlyings.clone().unwrap_or_default();
        let gmx_event_emitters = chain_config
            .gmx_event_emitters
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let gmx_market_tokens = chain_config
            .gmx_market_tokens
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        IngestConfig {
            chain,
            chain_id: chain.chain_id(),
            confirmations: 6,
            wrapped_native,
            profit_token_priority: priority,
            liquidation_protocol_aliases,
            c_token_underlyings,
            chainlink_feeds,
            savax: chain_config.savax,
            gmx_event_emitters,
            gmx_market_tokens,
            tip_sink_addresses: HashSet::new(),
            record_rejections: false,
            rpc_batch_blocks: 1,
            causal_window_txs: crate::explorer::classify::CAUSAL_WINDOW_TXS,
            causal_price_min_pct: crate::explorer::classify::CAUSAL_PRICE_MIN_PCT,
            jit_window_blocks: JIT_WINDOW_BLOCKS,
            interest_lookback_blocks: crate::explorer::interest_attr::LOOKBACK_BLOCKS,
            jit_pool_fee_bps: crate::explorer::classify::POOL_FEE_BPS,
        }
    }

    /// `from_chain` plus the explorer/backtest runtime overlays used by
    /// historical `explorer index` (CLI backfill).
    pub fn from_app_config(
        chain: ChainName,
        chain_config: &crate::config::ChainConfig,
        config: &crate::config::Config,
    ) -> Self {
        let mut cfg = Self::from_chain(chain, chain_config);
        cfg.confirmations = config.explorer.confirmations;
        cfg.rpc_batch_blocks = config.effective_rpc(chain).rpc_batch_blocks;
        cfg.record_rejections = config.backtest.record_rejections;
        cfg.tip_sink_addresses = config.explorer.tip_sink_addresses.iter().cloned().collect();
        cfg.causal_window_txs = config.explorer.causal_window_txs;
        cfg.causal_price_min_pct = config.explorer.causal_price_min_pct;
        cfg.jit_window_blocks = config.explorer.jit_window_blocks;
        cfg.interest_lookback_blocks = config.explorer.interest_lookback_blocks;
        cfg.jit_pool_fee_bps = config.explorer.jit_pool_fee_bps;
        cfg
    }
}

/// sAVAX exchange-rate reads, keyed by [`pricing::hour_bucket`].
/// A success is reused for the hour. A failure is remembered too: public
/// nodes prune historical state, and retrying `eth_call` on every block
/// serializes the backfill. Tagging falls back to route co-presence either way.
#[derive(Debug, Default)]
pub struct SavaxRateCache {
    by_hour: HashMap<u64, U256>,
    missed: HashSet<u64>,
}

impl SavaxRateCache {
    fn get(&self, hour: u64) -> Option<U256> {
        self.by_hour.get(&hour).copied()
    }

    fn insert(&mut self, hour: u64, rate: U256) {
        self.missed.remove(&hour);
        self.by_hour.insert(hour, rate);
    }

    fn missed(&self, hour: u64) -> bool {
        self.missed.contains(&hour)
    }

    fn miss(&mut self, hour: u64) {
        self.missed.insert(hour);
    }
}

/// Profit-token priority: the chain's stable majors (from `chains.toml`) then
/// wrapped native. An empty chain list falls through to the largest-delta
/// selection in `select_profit_token`.
fn build_profit_priority(
    chain_config: &crate::config::ChainConfig,
    wrapped_native: Address,
) -> Vec<Address> {
    let mut v = chain_config
        .profit_stable_tokens
        .clone()
        .unwrap_or_default();
    if !wrapped_native.is_zero() {
        v.push(wrapped_native);
    }
    v.retain(|a| !a.is_zero());
    v
}

/// Token0/token1 map plus V2-like skim-eligible and epoch-venue pairs.
#[derive(Debug, Clone, Copy)]
pub struct PoolViews<'a> {
    pub tokens: &'a HashMap<Address, (Address, Address)>,
    pub v2_like: &'a HashSet<Address>,
    /// Pharaoh / Blackhole pools for plan P3.15.
    pub epoch_venue: &'a HashSet<Address>,
}

/// Result of indexing one block.
#[derive(Debug, Clone)]
pub struct IndexedBlock {
    pub block: u64,
    pub ops: usize,
}

/// Outcome of a historical `run_range` backfill.
#[derive(Debug, Clone, Default)]
pub struct RangeOutcome {
    pub blocks_processed: u64,
    pub ops: u64,
    /// Blocks whose chunk fetch failed after retries and were skipped, sorted
    /// ascending. A re-run of the same range fills the gap via
    /// `unclassified_blocks`.
    pub skipped_blocks: Vec<u64>,
}

/// Fetch block + receipts (batched JSON-RPC POST).
pub async fn fetch_block_bundle(rpc: &RpcClient, block_number: u64) -> anyhow::Result<BlockBundle> {
    rpc.get_block_and_receipts_batch(block_number).await
}

/// Build the run-scoped classification context once per backfill session.
///
/// The registry sets are cloned into `Arc`s here and only the `Arc` is cloned
/// per block, so a multi-hour backfill does not copy the pool/feed universe
/// on every block.
fn run_block_context(cfg: &IngestConfig, pools: PoolViews<'_>) -> BlockContext {
    BlockContext {
        wrapped_native: cfg.wrapped_native,
        profit_policy: crate::explorer::profit::ProfitTokenPolicy {
            priority: cfg.profit_token_priority.clone(),
            wrapped_native: cfg.wrapped_native,
            weth: cfg.wrapped_native,
        },
        v2_like_pools: std::sync::Arc::new(pools.v2_like.clone()),
        chainlink_feeds: std::sync::Arc::new(cfg.chainlink_feeds.clone()),
        savax: cfg.savax,
        epoch_venue_pools: std::sync::Arc::new(pools.epoch_venue.clone()),
        gmx_event_emitters: std::sync::Arc::new(cfg.gmx_event_emitters.clone()),
        gmx_market_tokens: std::sync::Arc::new(cfg.gmx_market_tokens.clone()),
        causal_window_txs: cfg.causal_window_txs,
        causal_price_min_pct: cfg.causal_price_min_pct,
        jit_pool_fee_bps: cfg.jit_pool_fee_bps,
    }
}

/// Per-block indexing request for [`index_block`].
pub struct IndexBlockRequest<'a> {
    pub rpc: &'a RpcClient,
    pub store: &'a ExplorerStore,
    pub cfg: &'a IngestConfig,
    pub pools: PoolViews<'a>,
    pub ctx: &'a BlockContext,
    pub block_number: u64,
    pub native_price: Option<f64>,
    pub token_prices: Option<HashMap<Address, TokenUsd>>,
    pub savax_rates: &'a mut SavaxRateCache,
    pub qi_rates: &'a mut crate::explorer::benqi_liq::QiRateCache,
}

/// Fetch one block + receipts, decode, classify, persist. Idempotent: an
/// already-classified block is skipped. When `token_prices` is `None`, USD
/// prices for the block's profit tokens are warmed on demand.
pub async fn index_block(req: IndexBlockRequest<'_>) -> anyhow::Result<IndexedBlock> {
    let IndexBlockRequest {
        rpc,
        store,
        cfg,
        pools,
        ctx,
        block_number,
        native_price,
        token_prices,
        savax_rates,
        qi_rates,
    } = req;
    if store.block_classified(block_number)? {
        debug!(block = block_number, "already classified, skipping");
        return Ok(IndexedBlock {
            block: block_number,
            ops: 0,
        });
    }

    let bundle = fetch_block_bundle(rpc, block_number).await?;
    index_block_fetched(
        rpc,
        store,
        cfg,
        pools,
        FetchedBlockInput {
            block_number,
            native_price,
            token_prices,
            bundle,
            ctx,
        },
        savax_rates,
        qi_rates,
    )
    .await
}

struct DecodedTxs {
    tx_inputs: Vec<TxInput>,
    tx_rows: Vec<TxRow>,
    swap_rows: Vec<SwapRow>,
    transfer_rows: Vec<TransferRow>,
    jit_hashes: HashMap<u64, B256>,
}

fn decode_successful_txs(
    txs: &[TxData],
    receipts: &[ReceiptData],
    pools: PoolViews<'_>,
    cfg: &IngestConfig,
    base_fee_gwei: Option<f64>,
) -> DecodedTxs {
    let receipt_by_index: HashMap<u64, &ReceiptData> =
        receipts.iter().map(|r| (r.tx_index, r)).collect();
    let mut out = DecodedTxs {
        tx_inputs: Vec::with_capacity(txs.len()),
        tx_rows: Vec::with_capacity(txs.len()),
        swap_rows: Vec::new(),
        transfer_rows: Vec::new(),
        jit_hashes: HashMap::new(),
    };
    for tx in txs {
        let Some(receipt) = receipt_by_index.get(&tx.index).copied() else {
            continue;
        };
        if !receipt.status {
            continue;
        }
        let facts = classify::decode_tx_logs_with_aliases(
            tx.index,
            &receipt.logs,
            pools.tokens,
            &cfg.liquidation_protocol_aliases,
        );
        let max_priority_gwei = tx
            .max_priority_fee_per_gas
            .map(|p| p as f64 / 1e9)
            .unwrap_or(0.0);
        let effective_gwei = effective_gas_gwei(tx, receipt, base_fee_gwei, max_priority_gwei);
        let priority_gwei = if tx.max_priority_fee_per_gas.is_some() {
            max_priority_gwei.min(effective_gwei)
        } else {
            (effective_gwei - base_fee_gwei.unwrap_or(0.0)).max(0.0)
        };
        out.tx_rows.push(TxRow {
            hash: tx.hash,
            tx_index: tx.index,
            from: tx.from,
            to: tx.to,
            success: receipt.status,
            gas_used: receipt.gas_used,
            effective_gas_price_gwei: effective_gwei,
            priority_fee_gwei: priority_gwei,
            value: tx.value,
        });
        for s in &facts.swaps {
            out.swap_rows.push(SwapRow {
                tx_index: s.tx_index,
                log_index: s.log_index,
                pool: s.pool,
                amm: s.amm,
                token_in: s.token_in,
                token_out: s.token_out,
                amount_in: s.amount_in,
                amount_out: s.amount_out,
            });
        }
        for t in &facts.transfers {
            out.transfer_rows.push(TransferRow {
                tx_index: t.tx_index,
                log_index: t.log_index,
                token: t.token,
                from: t.from,
                to: t.to,
                amount: t.amount,
            });
        }
        if !facts.jit.is_empty() {
            out.jit_hashes.insert(tx.index, tx.hash);
        }
        out.tx_inputs.push(TxInput {
            tx_index: tx.index,
            tx_hash: tx.hash,
            from: tx.from,
            to: tx.to,
            success: receipt.status,
            gas_used: receipt.gas_used,
            effective_gas_price_gwei: effective_gwei,
            value: tx.value,
            transfers: facts.transfers,
            swaps: facts.swaps,
            liquidations: facts.liquidations,
            flashloans: facts.flashloans,
            buy_collaterals: facts.buy_collaterals,
            jit: facts.jit,
            v2_pair_ops: facts.v2_pair_ops,
            oracle_updates: facts.oracle_updates,
            reserve_updates: facts.reserve_updates,
            rate_cache_updates: facts.rate_cache_updates,
            keepers: facts.keepers,
            epoch_rewards: facts.epoch_rewards,
            gmx_events: facts.gmx_events,
        });
    }
    out
}

/// Prefer the receipt's `effectiveGasPrice`, then the tx's legacy `gasPrice`,
/// and only fall back to `base_fee + max_priority_fee` when the node omitted both.
fn effective_gas_gwei(
    tx: &TxData,
    receipt: &ReceiptData,
    base_fee_gwei: Option<f64>,
    max_priority_gwei: f64,
) -> f64 {
    receipt
        .effective_gas_price
        .or(tx.gas_price)
        .map(|p| p as f64 / 1e9)
        .unwrap_or_else(|| base_fee_gwei.unwrap_or(0.0).max(0.0) + max_priority_gwei)
}

fn persist_mode_b_lookback(store: &ExplorerStore, block_number: u64, txs: &[TxInput]) {
    let mut oracle_rows: Vec<(Address, i128)> = Vec::new();
    let mut reserve_rows: Vec<(Address, U256)> = Vec::new();
    for t in txs {
        for o in &t.oracle_updates {
            oracle_rows.push((o.feed, o.answer));
        }
        for r in &t.reserve_updates {
            reserve_rows.push((r.reserve, r.variable_borrow_rate));
        }
    }
    if let Err(e) = store.record_oracle_answers(block_number, &oracle_rows) {
        tracing::warn!(
            block = block_number,
            error = %e,
            "failed to persist oracle answers for mode-B lookback"
        );
    }
    if let Err(e) = store.record_reserve_rates(block_number, &reserve_rows) {
        tracing::warn!(
            block = block_number,
            error = %e,
            "failed to persist reserve rates for mode-B lookback"
        );
    }
}

/// Decode, classify, and persist a pre-fetched block bundle. Callers that
/// overlap RPC fetches must still invoke this in ascending block order so
/// JIT / oracle / interest lookbacks stay coherent.
pub async fn index_block_fetched(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pools: PoolViews<'_>,
    input: FetchedBlockInput<'_>,
    savax_rates: &mut SavaxRateCache,
    qi_rates: &mut crate::explorer::benqi_liq::QiRateCache,
) -> anyhow::Result<IndexedBlock> {
    let FetchedBlockInput {
        block_number,
        native_price,
        token_prices,
        bundle,
        ctx,
    } = input;
    let (block_data, txs, receipts) = bundle;
    let base_fee_gwei = block_data.base_fee_per_gas.map(|b| b as f64 / 1e9);
    let DecodedTxs {
        tx_inputs,
        tx_rows,
        swap_rows,
        transfer_rows,
        jit_hashes,
    } = decode_successful_txs(&txs, &receipts, pools, cfg, base_fee_gwei);

    let jit_cutoff = block_number.saturating_sub(cfg.jit_window_blocks);
    let latest = store.latest_classified_block()?;
    let rebuild_lifecycle =
        latest.is_some_and(|b| block_number <= b) || store.jit_block_exists(block_number)?;
    let open_positions = if rebuild_lifecycle {
        // Backfill/retry: state must be the one this block could have seen,
        // never the latest materialized table from later blocks.
        store.open_positions_as_of(block_number, jit_cutoff)?
    } else {
        store.open_positions(jit_cutoff)?
    };
    let interest_lookback = store
        .interest_lookback(block_number, cfg.interest_lookback_blocks)
        .unwrap_or_default();
    let feeds: Vec<Address> = cfg.chainlink_feeds.keys().copied().collect();
    let prior_oracle_answers = store
        .prior_oracle_answers(block_number, &feeds)
        .unwrap_or_default();
    let savax_exchange_rate_wad = fetch_savax_exchange_rate(
        rpc,
        cfg.savax,
        block_number,
        block_data.timestamp,
        savax_rates,
    )
    .await;
    let input = BlockInput {
        block: block_number,
        ts: block_data.timestamp,
        ctx: ctx.clone(),
        open_positions,
        interest_lookback,
        prior_oracle_answers,
        savax_exchange_rate_wad,
        tip_sinks: {
            let mut sinks = cfg.tip_sink_addresses.clone();
            if !block_data.coinbase.is_zero() {
                sinks.insert(block_data.coinbase);
            }
            sinks
        },
        txs: tx_inputs,
    };

    // Receipt-local, ordered LP journal for settled JIT reconciliation. The
    // parent is read from this exact header, never substituted from the last
    // stored journal (which could belong to an abandoned fork).
    let mut journal = LpBlock::from_input(&input, &block_data, &txs, &receipts);
    journal.parent_hash = parent_hash_for(rpc, &block_data).await;

    let classified = classify::classify_block(&input);
    blocking_store(|| -> anyhow::Result<()> {
        if cfg.record_rejections && !classified.rejects.is_empty() {
            let chain_str = cfg.chain.to_string();
            let run_id = format!("explorer-index-{block_number}");
            // One transaction for the block's reject set. Per-row insert errors
            // stay best-effort (logged and skipped, as before), but the
            // inserts are conflict-tolerant on `rejected_candidates`' natural key
            // (`schema.rs`), so a retry after a partial or failed pass converges
            // instead of appending a second copy. Rejects intentionally precede
            // `insert_block_facts`: if the facts fail, the retry re-derives them
            // identically and the unique key makes re-insertion a no-op.
            let tx = store.connection().unchecked_transaction()?;
            for r in &classified.rejects {
                if let Err(e) = store.insert_rejected_candidate(&run_id, &chain_str, r) {
                    tracing::warn!("explorer_classify reject insert failed: {e}");
                }
            }
            tx.commit()?;
        }
        Ok(())
    })?;
    let mut events: Vec<MevEvent> = classify::filter_unresolved(classified.events);
    classify::stamp_jit_tx_hashes(&mut events, &jit_hashes);
    let ops = events.len();

    // Reconcile JIT positions before any valuation: only receipt-backed
    // settled cashflows may become profit, and the old fee estimate is not a
    // basis. The window starts at the oldest position this block can close
    // (bounded by the configured window) and never includes blocks after this
    // one (backfill safety).
    let oldest_open = events
        .iter()
        .filter(|e| e.kind == MevKind::Jit)
        .filter_map(|e| e.details.get("opened_block").and_then(|v| v.as_u64()))
        .min()
        .unwrap_or(block_number)
        .max(block_number.saturating_sub(cfg.jit_window_blocks))
        .min(block_number);
    let mut window_blocks = store.jit_blocks_range(oldest_open, block_number.saturating_sub(1))?;
    window_blocks.push(journal.clone());
    window_blocks.sort_by_key(|b| b.block_number);
    window_blocks.retain(|b| b.block_number <= block_number);
    let mut archive = ArchiveBudget::new(rpc);
    jit_accounting::enrich(
        &mut events,
        &window_blocks,
        pools.tokens,
        cfg.chain_id,
        cfg.jit_window_blocks,
        &mut archive,
    )
    .await;

    blocking_store(|| persist_mode_b_lookback(store, block_number, &input.txs));

    // Benqi: seizeTokens → underlying before price warm / PnL (never price qi).
    crate::explorer::benqi_liq::convert_c_token_liquidation_events(
        rpc,
        &mut events,
        &cfg.c_token_underlyings,
        block_number,
        block_data.timestamp,
        qi_rates,
    )
    .await;

    let mut token_prices = match token_prices {
        Some(p) => p,
        None => {
            let tokens = event_tokens(&events);
            warm_prices_for_tokens(cfg, rpc, &tokens, block_data.timestamp, store).await?
        }
    };
    // Wrapped-native USD = CoinGecko native price (mevlive Price column parity).
    if let Some(np) = native_price {
        if !cfg.wrapped_native.is_zero() {
            token_prices.entry(cfg.wrapped_native).or_insert(TokenUsd {
                usd: np,
                decimals: 18,
            });
        }
    }
    // Price each JIT cashflow at its own hour, then evaluate with the same
    // evaluator repricing uses. Quotes persist to `prices` as usual.
    fill_jit_valuation(cfg, rpc, store, &mut events).await?;

    blocking_store(|| -> anyhow::Result<()> {
        store
            .insert_block_facts(BlockFactsInput {
                block_number,
                block_hash: &block_data.hash,
                ts: block_data.timestamp,
                base_fee_gwei,
                tx_count: txs.len(),
                txs: &tx_rows,
                swaps: &swap_rows,
                transfers: &transfer_rows,
                events: &events,
                native_price_usd: native_price,
                token_prices: &token_prices,
                jit_journal: Some(JitJournalInput {
                    block: &journal,
                    window_blocks: cfg.jit_window_blocks,
                    rebuild: rebuild_lifecycle,
                }),
            })
            .map(|_| ())
    })?;

    Ok(IndexedBlock {
        block: block_number,
        ops,
    })
}

/// Run a synchronous SQLite write on a blocking context when the runtime can
/// spare the worker.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, so the store cannot be
/// moved into `spawn_blocking` (the closure would need `'static` ownership of
/// a `Sync` value). `block_in_place` is the ownership-free equivalent: the
/// runtime replaces the worker while the statement runs. A current-thread
/// runtime has no spare worker, so tests take the inline path.
fn blocking_store<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Parent hash of exactly this header, hash-pinned. Stored journals are not a
/// substitute: after a reorg they can belong to a different fork. A missing
/// header leaves settlement unprovable rather than guessing.
async fn parent_hash_for(rpc: &RpcClient, block_data: &BlockData) -> Option<B256> {
    if block_data.number == 0 {
        return None;
    }
    rpc.parent_hash_at(block_data.hash, block_data.number)
        .await
        .ok()
}

/// Price every JIT cashflow at its own hour and run the shared evaluator.
///
/// The archive/ownership proof already ran during `enrich`; here only USD
/// quotes are attached. A missing quote clears the derived totals (NULL, not a
/// partial sum dressed as profit). Verified + complete rows are marked as
/// priced so strategy ROI can admit them; everything else stays approximate.
async fn fill_jit_valuation(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    store: &ExplorerStore,
    events: &mut [MevEvent],
) -> anyhow::Result<()> {
    let mut requests: BTreeMap<u64, BTreeSet<Address>> = BTreeMap::new();
    for ev in events.iter() {
        let Some(accounting) = JitAccounting::from_details(&ev.details) else {
            continue;
        };
        for (hour, tokens) in jit_accounting::quote_requests(&accounting) {
            requests.entry(hour).or_default().extend(tokens);
        }
    }
    let mut priced: HashMap<u64, HashMap<Address, (TokenUsd, String, u64)>> = HashMap::new();
    for (hour, tokens) in requests {
        let mut fetch: Vec<Address> = tokens.iter().copied().filter(|t| !t.is_zero()).collect();
        if tokens.contains(&Address::ZERO) && !cfg.wrapped_native.is_zero() {
            fetch.push(cfg.wrapped_native);
        }
        if fetch.is_empty() {
            continue;
        }
        let ts = hour.saturating_mul(3600);
        let quotes = warm_quotes_for_tokens(cfg, rpc, &fetch, ts, store).await?;
        priced.insert(hour, quotes);
    }

    for ev in events.iter_mut() {
        let Some(mut accounting) = JitAccounting::from_details(&ev.details) else {
            continue;
        };
        accounting.quotes.clear();
        for point in &accounting.timeline {
            if point.kind != "mint" && point.kind != "collect" {
                continue;
            }
            let hour = point.ts / 3600;
            for token in [accounting.token0, accounting.token1] {
                if let Some((q, source, observed)) = priced.get(&hour).and_then(|m| m.get(&token)) {
                    let provenance = if *observed == hour {
                        source.clone()
                    } else {
                        format!("{source}@{observed}")
                    };
                    accounting.quotes.push(Quote {
                        token,
                        hour,
                        usd: q.usd,
                        decimals: q.decimals,
                        source: provenance,
                    });
                }
            }
        }
        for gas in &accounting.gas {
            let hour = gas.ts / 3600;
            if let Some((q, _, _)) = priced.get(&hour).and_then(|m| m.get(&cfg.wrapped_native)) {
                // Key the same observation under the native marker so a later
                // repricing pass uses the exact quote this valuation used.
                store.put_price(Address::ZERO, hour, q.usd, "native_via_wrapped")?;
                accounting.quotes.push(Quote {
                    token: Address::ZERO,
                    hour,
                    usd: q.usd,
                    decimals: 18,
                    source: "native_via_wrapped".into(),
                });
            }
        }
        accounting.quotes.sort_by_key(|a| (a.token, a.hour));
        accounting
            .quotes
            .dedup_by(|a, b| a.token == b.token && a.hour == b.hour);
        accounting.valuation = jit_accounting::evaluate(&accounting);
        let complete = accounting.valuation.pricing_complete && accounting.status == "verified";
        ev.details["usd_approximate"] = serde_json::json!(!complete);
        ev.details["jit_accounting"] = serde_json::to_value(&accounting)
            .map_err(|e| anyhow::anyhow!("serialize JIT accounting: {e}"))?;
    }
    Ok(())
}

/// Effective chain head for indexing: head − confirmations.
pub async fn safe_head(rpc: &RpcClient, cfg: &IngestConfig) -> anyhow::Result<u64> {
    let head = rpc.get_block_number().await?;
    Ok(head.saturating_sub(cfg.confirmations))
}

/// Benqi sAVAX `getPooledAvaxByShares(1e18)` → AVAX wad per 1 sAVAX (plan P3.16).
async fn fetch_savax_exchange_rate(
    rpc: &RpcClient,
    savax: Option<Address>,
    block: u64,
    ts: u64,
    cache: &mut SavaxRateCache,
) -> Option<U256> {
    let savax = savax.filter(|a| !a.is_zero())?;
    let hour = pricing::hour_bucket(ts);
    if let Some(rate) = cache.get(hour) {
        return Some(rate);
    }
    if cache.missed(hour) {
        return None;
    }
    let mut data = crate::pool::selectors::SAVAX_GET_POOLED_AVAX_BY_SHARES.to_vec();
    data.extend_from_slice(&U256::from(10u64.pow(18)).to_be_bytes::<32>());
    match rpc.call(savax, Bytes::from(data), block).await {
        Ok(ret) if ret.len() >= 32 => {
            let rate = U256::from_be_slice(&ret[..32]);
            cache.insert(hour, rate);
            Some(rate)
        }
        Ok(_) => {
            cache.miss(hour);
            None
        }
        Err(e) => {
            debug!(block, "sAVAX exchangeRate eth_call failed: {e}");
            cache.miss(hour);
            None
        }
    }
}

fn known_token_cache(chain_id: u64) -> std::sync::Arc<crate::cache::TokenCache> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<u64, Arc<crate::cache::TokenCache>>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(chain_id)
        .or_insert_with(|| Arc::new(crate::cache::TokenCache::warm(chain_id)))
        .clone()
}

/// Outcome of a stats-window price fill.
pub async fn warm_prices_for_tokens(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    tokens: &[Address],
    ts: u64,
    store: &ExplorerStore,
) -> anyhow::Result<HashMap<Address, TokenUsd>> {
    Ok(warm_quotes_for_tokens(cfg, rpc, tokens, ts, store)
        .await?
        .into_iter()
        .map(|(token, (quote, _, _))| (token, quote))
        .collect())
}

/// Same warmer, retaining the observed hour and provenance for JIT evidence.
/// Initial valuation and repricing therefore use the same actual quote.
pub async fn warm_quotes_for_tokens(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    tokens: &[Address],
    ts: u64,
    store: &ExplorerStore,
) -> anyhow::Result<HashMap<Address, (TokenUsd, String, u64)>> {
    let mut out = HashMap::new();
    // Known-token decimals come from the bundled list via TokenCache::warm;
    // long-tail tokens get llama-reported decimals when present. Cache per
    // chain_id — this path runs once per indexed block.
    let known = known_token_cache(cfg.chain_id);
    let decimals_of = |a: &Address| -> Option<u32> {
        known
            .entries()
            .get(a)
            .and_then(|m| m.decimals.map(|d| u32::try_from(d).unwrap_or(18)))
    };

    let hour = pricing::hour_bucket(ts);
    let quotes = store.prices_at(tokens, hour)?;
    let mut priced: Vec<(Address, f64, String, u64)> = Vec::new();
    let mut missing: Vec<Address> = Vec::new();
    for t in tokens {
        if t.is_zero() || *t == crate::explorer::profit::NATIVE_MARKER {
            continue;
        }
        if let Some((usd, source, observed)) = quotes.get(t) {
            priced.push((*t, *usd, source.clone(), *observed));
        } else {
            missing.push(*t);
        }
    }
    // Llama-reported decimals stay alongside the price for resolution below.
    let mut llama_decimals: HashMap<Address, u32> = HashMap::new();
    if !missing.is_empty() {
        match pricing::fetch_prices_llama(cfg.chain, ts, &missing).await {
            Ok(prices) => {
                for (addr, (usd, llama_dec)) in prices {
                    // A non-positive quote is not a price: caching it would
                    // value every amount of this token in the hour at $0 and
                    // silently mark partial totals complete. Leave it unpriced.
                    if !usd.is_finite() || usd <= 0.0 {
                        warn!(
                            token = %format!("{addr:#x}"),
                            hour,
                            usd,
                            "llama returned a non-positive price; leaving token unpriced"
                        );
                        continue;
                    }
                    store.put_price(addr, hour, usd, "llama")?;
                    priced.push((addr, usd, "llama".into(), hour));
                    if let Some(d) = llama_dec {
                        llama_decimals.insert(addr, d);
                    }
                }
            }
            Err(e) => warn!("llama price fetch failed ({} tokens): {e}", missing.len()),
        }
    }

    // Resolve decimals: bundled cache → llama-reported decimals → on-chain
    // ERC-20 decimals via Multicall3 (long-tail fallback).
    let mut need_onchain: Vec<Address> = Vec::new();
    for (addr, usd, source, observed) in &priced {
        let dec = decimals_of(addr).or_else(|| llama_decimals.get(addr).copied());
        match dec {
            Some(d) => {
                out.insert(
                    *addr,
                    (
                        TokenUsd {
                            usd: *usd,
                            decimals: d,
                        },
                        source.clone(),
                        *observed,
                    ),
                );
            }
            None => need_onchain.push(*addr),
        }
    }
    if !need_onchain.is_empty() {
        let by_addr: HashMap<Address, (f64, String, u64)> = priced
            .iter()
            .map(|(a, usd, source, observed)| (*a, (*usd, source.clone(), *observed)))
            .collect();
        match crate::rpc::multicall::resolve_token_decimals(rpc, &need_onchain, 4).await {
            Ok(decs) => {
                for addr in need_onchain {
                    if let (Some(d), Some((usd, source, observed))) =
                        (decs.get(&addr), by_addr.get(&addr))
                    {
                        out.insert(
                            addr,
                            (
                                TokenUsd {
                                    usd: *usd,
                                    decimals: *d,
                                },
                                source.clone(),
                                *observed,
                            ),
                        );
                    }
                }
            }
            Err(e) => warn!("on-chain decimals resolution failed: {e:#}"),
        }
    }
    Ok(out)
}

/// Collect all token addresses appearing in events (for price warming).
pub fn event_tokens(events: &[MevEvent]) -> Vec<Address> {
    let priceable = |t: Address| !t.is_zero() && t != crate::explorer::profit::NATIVE_MARKER;
    let mut v: Vec<Address> = events
        .iter()
        .filter_map(|e| e.profit_token)
        .filter(|t| priceable(*t))
        .collect();
    // Every positive residual is summed at persist time, so each one needs a
    // real external price. Native residuals stay out of the warmer; persist
    // records `NATIVE_UNPRICED` instead of treating them as $0.
    for (tok, _) in events.iter().flat_map(|e| e.profit_tokens.iter()) {
        if priceable(*tok) {
            v.push(*tok);
        }
    }
    // Liquidation P&L needs the repaid debt asset priced too.
    for e in events.iter().filter(|e| e.kind == MevKind::Liquidation) {
        if let Some(a) = e
            .details
            .get("debt_asset")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
        {
            if !a.is_zero() {
                v.push(a);
            }
        }
    }
    // JIT settlement values both pool tokens; warm them at the block hour as
    // well so the base map (volume/display) is not silently missing them.
    for e in events.iter() {
        if let Some(a) = JitAccounting::from_details(&e.details) {
            for t in [a.token0, a.token1] {
                if priceable(t) {
                    v.push(t);
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Native-token USD price with hourly store cache (CoinGecko, then Llama).
pub async fn native_price_cached(
    cfg: &IngestConfig,
    store: &ExplorerStore,
    ts: u64,
) -> anyhow::Result<Option<f64>> {
    let hour = pricing::hour_bucket(ts);
    let marker = Address::ZERO; // native keyed at ZERO in the price table
    if let Some((usd, _)) = store.price_at(marker, hour)? {
        return Ok(Some(usd));
    }
    let fetched = match pricing::fetch_native_price_coingecko(cfg.chain).await {
        Ok(usd) => Some((usd, "coingecko")),
        Err(e) => {
            warn!("native price coingecko failed: {e}");
            match pricing::fetch_native_price_llama(cfg.chain, cfg.wrapped_native).await {
                Ok(usd) => Some((usd, "llama")),
                Err(e2) => {
                    warn!("native price llama failed: {e2}");
                    None
                }
            }
        }
    };
    if let Some((usd, source)) = fetched {
        // A non-positive fetch is not a usable quote. `put_price` would refuse
        // the write anyway; returning it here would still surface it as the
        // in-flight native price (gas at $0, inflated net P&L).
        if !usd.is_finite() || usd <= 0.0 {
            warn!("native price {source} returned a non-finite or non-positive quote {usd} for hour {hour}; treating it as unavailable");
            return Ok(None);
        }
        store.put_price(marker, hour, usd, source)?;
        // Also cache under wrapped-native so profit_usd converts for WAVAX/WETH.
        if !cfg.wrapped_native.is_zero() {
            store.put_price(cfg.wrapped_native, hour, usd, source)?;
        }
        Ok(Some(usd))
    } else {
        Ok(None)
    }
}

/// Historical / range backfill: index every block in `[from_block, to_block]`
/// (inclusive), clamped to `head − confirmations`. Idempotent via
/// `blocks_classified` and gap-resumable — an interrupted backfill can be
/// re-run over the same range and it will pick up where it left off.
///
/// When `block_concurrency > 1`, overlaps up to that many batched
/// block+receipts RPC fetches while classify/persist stays in ascending
/// block order (JIT / oracle / interest lookbacks require it).
///
/// Feeds the revenue report's 1d/7d/30d windows: without history in the store
/// the report has nothing to show for those windows.
///
/// Everything in the range is treated as finalized once it lags
/// `confirmations`. Token profit USD is priced historically (Llama keyed by
/// the block's timestamp inside `index_block`); native/gas USD uses the current
/// native price (a close approximation for ≤30d windows).
pub async fn run_range(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pools: PoolViews<'_>,
    range: RangeRequest,
    progress: &dyn JobProgress,
) -> anyhow::Result<RangeOutcome> {
    let RangeRequest {
        from_block,
        to_block,
        block_concurrency,
    } = range;
    let ctx = run_block_context(cfg, pools);
    if to_block < from_block {
        anyhow::bail!("to_block ({to_block}) < from_block ({from_block})");
    }
    // A failed head fetch cannot be skipped: nothing can be indexed without a
    // safe tip, so it must surface instead of clamping to `to_block`.
    let tip = rpc.get_block_number().await?;
    let safe_tip = tip.saturating_sub(cfg.confirmations);
    let end = to_block.min(safe_tip);
    if end < from_block {
        warn!(
            from_block,
            to_block,
            safe_tip,
            confirmations = cfg.confirmations,
            "range end is inside the confirmation lag — nothing to index yet"
        );
        return Ok(RangeOutcome::default());
    }

    let total = end - from_block + 1;
    let missing = store.unclassified_blocks(from_block, end)?;
    let mut outcome = RangeOutcome::default();
    let mut native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
    let t0 = std::time::Instant::now();
    let mut last_emit = t0;
    // Emit every concurrency stride or ~1s wall-clock, whichever comes first.
    let emit_stride = (block_concurrency.max(1) as u64).max(1);
    let mut emit_progress = |done: u64, ops: u64, force: bool| {
        let due = force
            || done == 0
            || done == total
            || done.is_multiple_of(emit_stride)
            || last_emit.elapsed() >= std::time::Duration::from_secs(1);
        if !due {
            return;
        }
        let mut evt = ProgressEvent::stage("backfill");
        evt.done = Some(done);
        evt.total = Some(total);
        evt.ops = Some(ops);
        evt.elapsed_ms = Some(t0.elapsed().as_millis() as u64);
        progress.emit(evt);
        last_emit = std::time::Instant::now();
    };
    emit_progress(0, 0, true);

    if missing.is_empty() {
        store.set_sync_state(cfg.chain_id, tip, end)?;
        if total > 0 {
            emit_progress(total, 0, true);
        }
        let mut done = ProgressEvent::stage("complete");
        done.ops = Some(0);
        done.elapsed_ms = Some(t0.elapsed().as_millis() as u64);
        progress.emit(done);
        return Ok(outcome);
    }

    let mut savax_rates = SavaxRateCache::default();
    let mut qi_rates = crate::explorer::benqi_liq::QiRateCache::default();
    // Blocks dropped because their fetch/index failed after retries. Both the
    // serial and pipelined paths record them so a re-run fills the gap.
    let mut skipped: BTreeSet<u64> = BTreeSet::new();
    let cap = block_concurrency.max(1).min(missing.len());
    // One POST of `chunk` blocks. In-flight blocks stay at `cap`: a chunk of
    // 32 with concurrency 32 is one POST, not 32×32.
    let chunk = cfg.rpc_batch_blocks.max(1).min(cap);
    if cap <= 1 {
        for &block in &missing {
            if block.saturating_sub(from_block) % 256 == 0 {
                native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
            }
            let indexed = match index_block(IndexBlockRequest {
                rpc,
                store,
                cfg,
                pools,
                ctx: &ctx,
                block_number: block,
                native_price,
                token_prices: None,
                savax_rates: &mut savax_rates,
                qi_rates: &mut qi_rates,
            })
            .await
            {
                Ok(indexed) => indexed,
                Err(e) => {
                    warn!(
                        block,
                        error = %e,
                        "backfill block failed after retries; skipping \
                         (re-run the same range later to fill the gap)"
                    );
                    skipped.insert(block);
                    let done = block - from_block + 1;
                    emit_progress(done, outcome.ops, done == total);
                    continue;
                }
            };
            outcome.blocks_processed += 1;
            outcome.ops += indexed.ops as u64;
            let done = block - from_block + 1;
            emit_progress(done, outcome.ops, done == total);
        }
    } else {
        let posts = (cap / chunk).max(1);
        let sem = Arc::new(Semaphore::new(posts));
        let mut inflight = FuturesUnordered::new();
        let mut ready: BTreeMap<u64, BlockBundle> = BTreeMap::new();
        let mut next_spawn = 0usize;
        let mut next_persist = 0usize;

        let spawn_one =
            |next_spawn: &mut usize, inflight: &mut FuturesUnordered<_>| -> anyhow::Result<()> {
                if *next_spawn >= missing.len() {
                    return Ok(());
                }
                let end = (*next_spawn + chunk).min(missing.len());
                let blocks = missing[*next_spawn..end].to_vec();
                *next_spawn = end;
                let sem = sem.clone();
                let rpc = rpc.clone();
                inflight.push(async move {
                    // Return the chunk's block list alongside the result so a
                    // failed fetch is attributable to blocks that can be skipped.
                    let chunk_blocks = blocks.clone();
                    let result = async {
                        let _permit = sem
                            .acquire_owned()
                            .await
                            .map_err(|e| anyhow::anyhow!("backfill fetch semaphore closed: {e}"))?;
                        let rows = if blocks.len() == 1 {
                            vec![(blocks[0], fetch_block_bundle(&rpc, blocks[0]).await?)]
                        } else {
                            let fetched = rpc.get_blocks_and_receipts_batch(&blocks).await?;
                            if fetched.len() != blocks.len() {
                                anyhow::bail!(
                                    "batch returned {} blocks, expected {}",
                                    fetched.len(),
                                    blocks.len()
                                );
                            }
                            blocks.into_iter().zip(fetched).collect()
                        };
                        Ok::<_, anyhow::Error>(rows)
                    }
                    .await;
                    (chunk_blocks, result)
                });
                Ok(())
            };

        while next_spawn < missing.len() && inflight.len() < posts {
            spawn_one(&mut next_spawn, &mut inflight)?;
        }

        while next_persist < missing.len() {
            let need = missing[next_persist];
            if skipped.contains(&need) {
                next_persist += 1;
                continue;
            }
            if let Some(bundle) = ready.remove(&need) {
                if need.saturating_sub(from_block) % 256 == 0 {
                    native_price =
                        native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
                }
                let indexed = index_block_fetched(
                    rpc,
                    store,
                    cfg,
                    pools,
                    FetchedBlockInput {
                        block_number: need,
                        native_price,
                        token_prices: None,
                        bundle,
                        ctx: &ctx,
                    },
                    &mut savax_rates,
                    &mut qi_rates,
                )
                .await?;
                outcome.blocks_processed += 1;
                outcome.ops += indexed.ops as u64;
                next_persist += 1;
                let done = need - from_block + 1;
                emit_progress(done, outcome.ops, done == total);
                while next_spawn < missing.len() && inflight.len() < posts {
                    spawn_one(&mut next_spawn, &mut inflight)?;
                }
                continue;
            }

            match inflight.next().await {
                Some((_chunk_blocks, Ok(rows))) => {
                    for (block, bundle) in rows {
                        ready.insert(block, bundle);
                    }
                    while next_spawn < missing.len() && inflight.len() < posts {
                        spawn_one(&mut next_spawn, &mut inflight)?;
                    }
                }
                Some((chunk_blocks, Err(e))) => {
                    // The fetch path already exhausted `retry_call` backoff.
                    // Skip the chunk, keep the pipeline full, and let a re-run
                    // heal the gap via `unclassified_blocks`.
                    warn!(
                        blocks = ?chunk_blocks,
                        error = %e,
                        "backfill chunk fetch failed after retries; skipping blocks \
                         (re-run the same range later to fill the gap)"
                    );
                    skipped.extend(chunk_blocks);
                    // Refill exactly like the success branch: without this the
                    // loop can drain `inflight` while `next_spawn` is still
                    // pending and bail out as stalled.
                    while next_spawn < missing.len() && inflight.len() < posts {
                        spawn_one(&mut next_spawn, &mut inflight)?;
                    }
                }
                None => anyhow::bail!(
                    "backfill fetch pipeline stalled waiting for block {need} \
                     (spawned {next_spawn}/{}, ready {})",
                    missing.len(),
                    ready.len()
                ),
            }
        }
    }

    outcome.skipped_blocks = skipped.into_iter().collect();
    store.set_sync_state(cfg.chain_id, tip, end)?;
    if total > 0 {
        // Report the blocks actually indexed: a run with gaps must not look
        // 100% complete.
        emit_progress(outcome.blocks_processed, outcome.ops, true);
    }
    let mut done = ProgressEvent::stage("complete");
    done.ops = Some(outcome.ops);
    done.elapsed_ms = Some(t0.elapsed().as_millis() as u64);
    progress.emit(done);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::MevKind;
    use alloy::primitives::U256;

    #[test]
    fn blocking_store_is_inline_outside_a_runtime() {
        assert_eq!(blocking_store(|| 7), 7);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_store_runs_on_a_multi_thread_runtime() {
        assert_eq!(blocking_store(|| 7), 7);
    }

    #[test]
    fn savax_rate_cache_reuses_one_success_per_hour() {
        let mut cache = SavaxRateCache::default();
        let ts = 1_700_000_000u64;
        let hour = pricing::hour_bucket(ts);
        assert_eq!(hour, pricing::hour_bucket(ts + 1800));
        assert!(cache.get(hour).is_none());
        cache.insert(hour, U256::from(42u64));
        assert_eq!(cache.get(hour), Some(U256::from(42u64)));
        assert!(cache.get(pricing::hour_bucket(ts + 3600)).is_none());
        cache.miss(hour);
        cache.insert(hour, U256::from(7u64));
        assert!(!cache.missed(hour));
        assert_eq!(cache.get(hour), Some(U256::from(7u64)));
        let next = pricing::hour_bucket(ts + 3600);
        cache.miss(next);
        assert!(cache.missed(next));
        assert!(cache.get(next).is_none());
    }

    #[test]
    fn profit_priority_polygon() {
        let cfg = crate::config::defaults::default_chains()
            .remove("polygon")
            .unwrap();
        let ic = IngestConfig::from_chain(ChainName::Polygon, &cfg);
        assert_eq!(ic.chain_id, 137);
        assert_eq!(ic.profit_token_priority.len(), 5);
        assert!(!ic.wrapped_native.is_zero());
    }

    /// P0.2 config wiring: the Address-keyed `liquidation_protocol_aliases`
    /// table in `chains.toml` deserializes and reaches the ingest config —
    /// Avalanche carries the Benqi registry, Ethereum keeps Spark, Polygon
    /// has none configured.
    #[test]
    fn liquidation_protocol_aliases_from_chain_config() {
        use alloy::primitives::address;
        let chains = crate::config::defaults::default_chains();
        let avax = IngestConfig::from_chain(ChainName::Avalanche, &chains["avalanche"]);
        assert_eq!(
            avax.liquidation_protocol_aliases
                .get(&address!("5c0401e81bc07ca70fad469b451682c0d747ef1c"))
                .copied(),
            Some("benqi")
        );
        // Benqi qi → underlying map (core + isolated).
        assert_eq!(avax.c_token_underlyings.len(), 22);
        assert_eq!(
            avax.c_token_underlyings
                .get(&address!("5c0401e81bc07ca70fad469b451682c0d747ef1c"))
                .copied(),
            Some(address!("B31f66AA3C1e785363F0875A1B74E27b85FD66c7")) // WAVAX
        );
        assert_eq!(
            avax.c_token_underlyings
                .get(&address!("89a415b3d20098e6a6c8f7a59001c67bd3129821"))
                .copied(),
            Some(address!("152b9d0FdC40C096757F570A51E494bd4b943E50")) // BTC.b
        );
        assert_eq!(
            avax.liquidation_protocol_aliases
                .get(&address!("4F01AeD16D97E3aB5ab2B501154DC9bb0F1A5A2C"))
                .copied(),
            Some("aave_v2")
        );
        // aave_v2_pool + 22 Benqi aliases.
        assert_eq!(avax.liquidation_protocol_aliases.len(), 23);
        let eth = IngestConfig::from_chain(ChainName::Ethereum, &chains["ethereum"]);
        assert_eq!(
            eth.liquidation_protocol_aliases
                .get(&address!("c13e21b648a5ee794902342038ff3adab66be987"))
                .copied(),
            Some("spark")
        );
        assert!(eth.c_token_underlyings.is_empty());
        let poly = IngestConfig::from_chain(ChainName::Polygon, &chains["polygon"]);
        assert_eq!(
            poly.liquidation_protocol_aliases
                .get(&address!("8dFf5E27EA6b7AC08EbFdf9eB090F32ee9a30fcf"))
                .copied(),
            Some("aave_v2")
        );
        assert_eq!(poly.liquidation_protocol_aliases.len(), 1);
        assert!(poly.c_token_underlyings.is_empty());
        let bsc = IngestConfig::from_chain(ChainName::Bsc, &chains["bsc"]);
        assert_eq!(bsc.liquidation_protocol_aliases.len(), 55);
        assert!(bsc
            .liquidation_protocol_aliases
            .values()
            .all(|l| *l == "venus"));
        assert_eq!(bsc.c_token_underlyings.len(), 54); // vBNB has no underlying()
        assert_eq!(
            bsc.c_token_underlyings
                .get(&address!("eca88125a5adbe82614ffc12d0db554e2e2867c8"))
                .copied(),
            Some(address!("8ac76a51cc950d9822d68b83fe1ad97b32cd580d")) // USDC
        );
    }

    /// P1.4 config wiring: Avalanche ships Chainlink feed → asset map.
    #[test]
    fn chainlink_feeds_from_chain_config() {
        use alloy::primitives::address;
        let chains = crate::config::defaults::default_chains();
        let avax = IngestConfig::from_chain(ChainName::Avalanche, &chains["avalanche"]);
        assert!(avax.chainlink_feeds.len() >= 5);
        assert_eq!(
            avax.chainlink_feeds
                .get(&address!("0x0A77230d17318075983913bC2145DB16C7366156"))
                .copied(),
            Some(address!("0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"))
        );
        let poly = IngestConfig::from_chain(ChainName::Polygon, &chains["polygon"]);
        assert!(poly.chainlink_feeds.is_empty());
    }

    #[test]
    fn event_tokens_dedup() {
        let a = Address::new([4u8; 20]);
        let mut ev = sample();
        ev.profit_token = Some(a);
        let ev2 = ev.clone();
        let toks = event_tokens(&[ev, ev2]);
        assert_eq!(toks, vec![a]);
    }

    #[test]
    fn event_tokens_includes_every_residual() {
        let primary = Address::new([4u8; 20]);
        let residual = Address::new([5u8; 20]);
        let mut ev = sample();
        ev.profit_token = Some(primary);
        ev.profit_tokens = vec![
            (primary, U256::from(1u64)),
            (residual, U256::from(2u64)),
            (crate::explorer::profit::NATIVE_MARKER, U256::from(3u64)),
        ];
        let toks = event_tokens(&[ev]);
        assert_eq!(toks, vec![primary, residual]);
    }

    fn sample() -> MevEvent {
        MevEvent {
            block: 1,
            ts: 0,
            tx_index: 0,
            tx_hash: B256::ZERO,
            kind: MevKind::ArbAtomic,
            searcher: Address::ZERO,
            contract: None,
            pools: vec![],
            profit_token: None,
            profit_amount: None,
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: crate::explorer::types::Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({}),
        }
    }
}
