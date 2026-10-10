//! Explorer ingest — block/receipt fetch, decode, classify, persist.
//!
//! Consumes the same `get_block_and_receipts_batch` path the scanner uses,
//! decodes logs into explorer facts, runs the classifier, and persists into
//! the explorer store. Idempotent per block (checkpoints in
//! `blocks_classified`); reorg-aware via stored block hashes; confirmation
//! lag applied before indexing.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, B256, U256};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::data::{BlockData, ReceiptData, TxData};
use crate::explorer::classify::{self, BlockInput, TxInput};
use crate::explorer::pricing::{self, TokenUsd};
use crate::explorer::store::{
    BlockFactsInput, ExplorerStore, OpenPosition, SwapRow, TransferRow, TxRow,
};
use crate::explorer::types::{MevEvent, MevKind};
use crate::progress::{JobProgress, ProgressEvent};
use crate::rpc::RpcClient;
use crate::types::ChainName;

/// Block header + txs + receipts from a single batched RPC fetch.
pub type BlockBundle = (BlockData, Vec<TxData>, Vec<ReceiptData>);

/// Per-block fetch payload for [`index_block_fetched`] (not classify [`BlockInput`]).
#[derive(Debug)]
pub struct FetchedBlockInput {
    pub block_number: u64,
    pub native_price: Option<f64>,
    pub token_prices: Option<HashMap<Address, TokenUsd>>,
    pub bundle: BlockBundle,
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
    /// Mevlive-parity fallback for `arb_atomic`. Default false —
    /// only closed multi-pool cycles are labeled arb (spec /).
    pub arb_likely_parity: bool,
    /// Emitter-address → protocol label relabels for liquidation events whose
    /// topic0 is shared with another protocol (Aave-V3 ABI: Spark; Compound V2
    /// ABI: Benqi qiTokens — plan P0.2 /).
    pub liquidation_protocol_aliases: HashMap<Address, &'static str>,
    /// Chainlink aggregator → underlying asset (plan P1.1 / P1.4).
    pub chainlink_feeds: HashMap<Address, Address>,
    pub savax: Option<Address>,
    pub gmx_event_emitters: HashSet<Address>,
}

impl IngestConfig {
    pub fn from_chain(chain: ChainName, chain_config: &crate::config::ChainConfig) -> Self {
        let wrapped_native = chain_config.wrapped_native_token.unwrap_or(Address::ZERO);
        let priority = build_profit_priority(chain, wrapped_native);
        // Config labels are deserialized as owned `String`s; intern them once
        // here so the per-log remap stays a `&'static str` pointer compare.
        let intern = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        let mut liquidation_protocol_aliases = HashMap::new();
        if let Some(spark) = chain_config.spark_pool {
            liquidation_protocol_aliases.insert(spark, "spark");
        }
        if let Some(aliases) = &chain_config.liquidation_protocol_aliases {
            liquidation_protocol_aliases.extend(aliases.iter().map(|(a, l)| (*a, intern(l))));
        }
        let chainlink_feeds = chain_config.chainlink_feeds.clone().unwrap_or_default();
        let gmx_event_emitters = chain_config
            .gmx_event_emitters
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
            arb_likely_parity: false,
            liquidation_protocol_aliases,
            chainlink_feeds,
            savax: chain_config.savax,
            gmx_event_emitters,
        }
    }
}

/// Profit-token priority: USDC → USDT → DAI → wrapped native.
/// Canonical addresses from `known_tokens.json`; chain-specific via
/// `wrapped_native`. Unknown-chain long-tail falls through to the
/// largest-delta fallback in `select_profit_token`.
fn build_profit_priority(chain: ChainName, wrapped_native: Address) -> Vec<Address> {
    let parse = |s: &str| s.parse::<Address>().unwrap_or(Address::ZERO);
    let mut v = match chain {
        ChainName::Polygon => vec![
            parse("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"), // USDC (native)
            parse("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"), // USDC.e
            parse("0xc2132D05D31c914a87C6611C10748AEb04B58e8F"), // USDT
            parse("0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063"), // DAI
        ],
        ChainName::Avalanche => vec![
            parse("0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E"), // USDC
            parse("0x9702230A8Ea53601f5cD2dc00fDBc13d4dF19497"), // USDC.e
            parse("0xc7198437980c041389c85c43e942b31037ADb125"), // USDT.e
            parse("0xd586E7F844cEa2F50bf48A84c291e3C71F0fDa99"), // DAI.e
        ],
        ChainName::Bsc => vec![
            parse("0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d"), // USDC
            parse("0x55d398326f99059fF775485246999027B3197955"), // USDT
            parse("0x1AF3F329e8BE154074D8769D1FFa4eE058B1DBc3"), // DAI
        ],
        ChainName::Ethereum => vec![
            parse("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
            parse("0xdAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
            parse("0x6B175474E89094C44Da98b954EedeAC495271d0F"), // DAI
        ],
        ChainName::Arbitrum => vec![
            parse("0xaf88d065e77c8cC2239327C5EDb3A432268e5831"), // USDC
            parse("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"), // USDT
            parse("0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1"), // DAI
        ],
        ChainName::Base => vec![
            parse("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"), // USDC
            parse("0x4200000000000000000000000000000000000006"), // WETH (no USDT/DAI majors)
        ],
        ChainName::Optimism => vec![
            parse("0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85"), // USDC
            parse("0x94b008aA00579c1307B0EF2c499aD98a8ce58e58"), // USDT
            parse("0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1"), // DAI
        ],
    };
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
    pub events: usize,
    pub ops: usize,
}

/// Outcome of a historical `run_range` backfill.
#[derive(Debug, Clone, Default)]
pub struct RangeOutcome {
    pub blocks_processed: u64,
    pub ops: u64,
}

/// Fetch block + receipts (batched JSON-RPC POST).
pub async fn fetch_block_bundle(rpc: &RpcClient, block_number: u64) -> anyhow::Result<BlockBundle> {
    rpc.get_block_and_receipts_batch(block_number).await
}

/// Fetch one block + receipts, decode, classify, persist. Idempotent: an
/// already-classified block is skipped. When `token_prices` is `None`, USD
/// prices for the block's profit tokens are warmed on demand.
pub async fn index_block(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pools: PoolViews<'_>,
    block_number: u64,
    native_price: Option<f64>,
    token_prices: Option<HashMap<Address, TokenUsd>>,
) -> anyhow::Result<IndexedBlock> {
    if store.block_classified(block_number)? {
        debug!(block = block_number, "already classified, skipping");
        return Ok(IndexedBlock {
            block: block_number,
            events: 0,
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
        },
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
        let (
            transfers,
            swaps,
            liquidations,
            flashloans,
            buy_collaterals,
            jit,
            v2_pair_ops,
            oracle_updates,
            reserve_updates,
            rate_cache_updates,
            keepers,
            epoch_rewards,
            gmx_events,
            user_ops,
        ) = classify::decode_tx_logs_with_aliases(
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
        for s in &swaps {
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
        for t in &transfers {
            out.transfer_rows.push(TransferRow {
                tx_index: t.tx_index,
                log_index: t.log_index,
                token: t.token,
                from: t.from,
                to: t.to,
                amount: t.amount,
            });
        }
        if !jit.is_empty() {
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
            transfers,
            swaps,
            liquidations,
            flashloans,
            buy_collaterals,
            jit,
            v2_pair_ops,
            oracle_updates,
            reserve_updates,
            rate_cache_updates,
            keepers,
            epoch_rewards,
            gmx_events,
            user_ops,
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
    input: FetchedBlockInput,
) -> anyhow::Result<IndexedBlock> {
    let FetchedBlockInput {
        block_number,
        native_price,
        token_prices,
        bundle,
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

    let open_positions = store.open_positions(block_number.saturating_sub(JIT_WINDOW_BLOCKS))?;
    let interest_lookback = store
        .interest_lookback(
            block_number,
            crate::explorer::interest_attr::LOOKBACK_BLOCKS,
        )
        .unwrap_or_default();
    let feeds: Vec<Address> = cfg.chainlink_feeds.keys().copied().collect();
    let prior_oracle_answers = store
        .prior_oracle_answers(block_number, &feeds)
        .unwrap_or_default();
    let savax_exchange_rate_wad = fetch_savax_exchange_rate(rpc, cfg.savax, block_number).await;
    let input = BlockInput {
        block: block_number,
        ts: block_data.timestamp,
        wrapped_native: cfg.wrapped_native,
        profit_policy: crate::explorer::profit::ProfitTokenPolicy {
            priority: cfg.profit_token_priority.clone(),
            wrapped_native: cfg.wrapped_native,
            weth: cfg.wrapped_native,
        },
        arb_likely_parity: cfg.arb_likely_parity,
        open_positions,
        v2_like_pools: pools.v2_like.clone(),
        chainlink_feeds: cfg.chainlink_feeds.clone(),
        savax: cfg.savax,
        epoch_venue_pools: pools.epoch_venue.clone(),
        gmx_event_emitters: cfg.gmx_event_emitters.clone(),
        interest_lookback,
        prior_oracle_answers,
        savax_exchange_rate_wad,
        txs: tx_inputs,
    };

    let mut events: Vec<MevEvent> = classify::filter_unresolved(classify::classify_block(&input));
    classify::stamp_jit_tx_hashes(&mut events, &jit_hashes);
    let ops = events.len();

    persist_mode_b_lookback(store, block_number, &input.txs);

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

    store.insert_block_facts(BlockFactsInput {
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
    })?;

    persist_jit_positions(store, block_number, &input.txs)?;

    Ok(IndexedBlock {
        block: block_number,
        events: txs.len(),
        ops,
    })
}

/// Persist JIT open positions: record Mints not closed in the same
/// block, delete rows for Burns, and prune positions outside the window.
fn persist_jit_positions(
    store: &ExplorerStore,
    block_number: u64,
    txs: &[TxInput],
) -> anyhow::Result<()> {
    let key = |j: &crate::explorer::types::JitFact| (j.pool, j.owner, j.tick_lower, j.tick_upper);
    for t in txs {
        let burned: std::collections::HashSet<_> =
            t.jit.iter().filter(|j| !j.is_mint).map(key).collect();
        for j in t.jit.iter().filter(|j| j.is_mint) {
            if burned.contains(&key(j)) {
                continue; // same-block JIT, not an open position
            }
            store.record_jit_open(&[OpenPosition {
                pool: j.pool,
                owner: j.owner,
                tick_lower: j.tick_lower,
                tick_upper: j.tick_upper,
                opened_block: block_number,
                liquidity: j.liquidity,
            }])?;
        }
        for j in t.jit.iter().filter(|j| !j.is_mint) {
            store.close_jit_position(j.pool, j.owner, j.tick_lower, j.tick_upper)?;
        }
    }
    store.prune_jit_positions(block_number.saturating_sub(JIT_WINDOW_BLOCKS))?;
    Ok(())
}

/// Effective chain head for indexing: head − confirmations.
pub async fn safe_head(rpc: &RpcClient, cfg: &IngestConfig) -> anyhow::Result<u64> {
    let head = rpc.get_block_number().await?;
    Ok(head.saturating_sub(cfg.confirmations))
}

/// Reorg check: stored hash at `block` differs from chain → unwind from there.
/// Returns Some(fork_block) when a reorg was detected and unwound.
pub async fn check_reorg(
    rpc: &RpcClient,
    store: &ExplorerStore,
    block: u64,
) -> anyhow::Result<Option<u64>> {
    let Some(stored) = store.block_hash(block)? else {
        return Ok(None);
    };
    let (block_data, _, _) = match rpc.get_block_and_receipts_batch(block).await {
        Ok(v) => v,
        Err(e) => {
            warn!(block, "reorg-check fetch failed: {e}");
            return Ok(None);
        }
    };
    let onchain: B256 = block_data.hash;
    if stored != format!("{onchain:#x}") {
        warn!(block, "reorg detected — unwinding from fork block");
        store.unwind_from(block)?;
        Ok(Some(block))
    } else {
        Ok(None)
    }
}

/// Cheap per-poll reorg re-verify: a single header-only hash compare
/// on the highest already-indexed block. Runs on every live poll, so a reorg of
/// the indexed tip is caught before the indexer extends onto it; the heavier
/// 32-block `check_reorg` sweep (block + receipts) stays to catch deeper forks.
pub async fn verify_indexed_tip(
    rpc: &RpcClient,
    store: &ExplorerStore,
    block: u64,
) -> anyhow::Result<Option<u64>> {
    let Some(stored) = store.block_hash(block)? else {
        return Ok(None);
    };
    match rpc.get_block_hash(block).await {
        Ok(onchain) if stored != format!("{onchain:#x}") => {
            warn!(
                block,
                "reorg detected via indexed-tip hash compare — unwinding"
            );
            store.unwind_from(block)?;
            Ok(Some(block))
        }
        Ok(_) => Ok(None),
        Err(e) => {
            warn!(block, "indexed-tip reorg check failed: {e}");
            Ok(None)
        }
    }
}

/// Benqi sAVAX `getPooledAvaxByShares(1e18)` → AVAX wad per 1 sAVAX (plan P3.16).
async fn fetch_savax_exchange_rate(
    rpc: &RpcClient,
    savax: Option<Address>,
    block: u64,
) -> Option<U256> {
    let savax = savax.filter(|a| !a.is_zero())?;
    let mut data = crate::pool::selectors::SAVAX_GET_POOLED_AVAX_BY_SHARES.to_vec();
    data.extend_from_slice(&U256::from(10u64.pow(18)).to_be_bytes::<32>());
    match rpc.call(savax, Bytes::from(data), block).await {
        Ok(ret) if ret.len() >= 32 => Some(U256::from_be_slice(&ret[..32])),
        Ok(_) => None,
        Err(e) => {
            debug!(block, "sAVAX exchangeRate eth_call failed: {e}");
            None
        }
    }
}

/// Outcome of a stats-window price fill.
pub async fn warm_prices_for_tokens(
    cfg: &IngestConfig,
    rpc: &RpcClient,
    tokens: &[Address],
    ts: u64,
    store: &ExplorerStore,
) -> anyhow::Result<HashMap<Address, TokenUsd>> {
    let mut out = HashMap::new();
    // Known-token decimals come from the bundled list via TokenCache::warm;
    // long-tail tokens get llama-reported decimals when present.
    let known = crate::cache::TokenCache::warm(cfg.chain_id);
    let decimals_of = |a: &Address| -> Option<u32> {
        known
            .entries()
            .get(a)
            .and_then(|m| m.decimals.map(|d| u32::try_from(d).unwrap_or(18)))
    };

    let hour = pricing::hour_bucket(ts);
    // Priced tokens (store cache first, then llama), keyed by address.
    let mut priced: Vec<(Address, f64)> = Vec::new();
    let mut missing: Vec<Address> = Vec::new();
    for t in tokens {
        if t.is_zero() || *t == crate::explorer::profit::NATIVE_MARKER {
            continue;
        }
        if let Some((usd, _)) = store.price_at(*t, hour)? {
            priced.push((*t, usd));
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
                    store.put_price(addr, hour, usd, "llama")?;
                    priced.push((addr, usd));
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
    for (addr, usd) in &priced {
        let dec = decimals_of(addr).or_else(|| llama_decimals.get(addr).copied());
        match dec {
            Some(d) => {
                out.insert(
                    *addr,
                    TokenUsd {
                        usd: *usd,
                        decimals: d,
                    },
                );
            }
            None => need_onchain.push(*addr),
        }
    }
    if !need_onchain.is_empty() {
        let usd_of = |a: &Address| priced.iter().find(|(p, _)| p == a).map(|(_, u)| *u);
        match crate::rpc::multicall::resolve_token_decimals(rpc, &need_onchain, 4).await {
            Ok(decs) => {
                for addr in need_onchain {
                    if let (Some(d), Some(usd)) = (decs.get(&addr), usd_of(&addr)) {
                        out.insert(addr, TokenUsd { usd, decimals: *d });
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

/// Live streaming mode: on start, jump to the current confirmed tip and only
/// follow new blocks forward. Never resumes a historical `indexed_to` gap and
/// never walks earlier blocks. Runs until `stop` is set; the indexed tip is
/// hash-verified on every poll and a heavier reorg sweep runs every
/// 32 blocks.
pub async fn run_live(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pools: PoolViews<'_>,
    poll_ms: u64,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<u64> {
    use std::sync::atomic::Ordering;

    let mut indexed_total: u64 = 0;
    let mut since_reorg_check: u64 = 0;
    // `None` until the first successful tip probe — then pinned to tip, not DB.
    let mut next_block: Option<u64> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let safe = match safe_head(rpc, cfg).await {
            Ok(v) => v,
            Err(e) => {
                warn!("safe_head failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                continue;
            }
        };

        let mut cursor = match next_block {
            Some(n) => n,
            None => {
                // Tip-only start: ignore stored `indexed_to` so we never catch up
                // historical backlog. Anchor the checkpoint just behind tip.
                warn!(
                    tip = safe,
                    confirmations = cfg.confirmations,
                    "live indexer starting at current tip (no historical catch-up)"
                );
                store.set_sync_state(cfg.chain_id, safe, safe.saturating_sub(1))?;
                safe
            }
        };

        if safe >= cursor {
            // cheap per-poll reorg re-verify on the last indexed block.
            // Only meaningful for blocks we indexed this session (checkpoint is
            // tip-anchored on start).
            let indexed_to = store.get_indexed_to(cfg.chain_id)?;
            if indexed_to > 0
                && indexed_to < cursor
                && verify_indexed_tip(rpc, store, indexed_to).await?.is_some()
            {
                cursor = indexed_to;
            }

            since_reorg_check += 1;
            if since_reorg_check >= 32 {
                let check_at = cursor.saturating_sub(1);
                if check_reorg(rpc, store, check_at).await?.is_some() {
                    cursor = check_at;
                }
                since_reorg_check = 0;
            }

            let native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
            for block in cursor..=safe {
                let indexed =
                    index_block(rpc, store, cfg, pools, block, native_price, None).await?;
                indexed_total += indexed.ops as u64;
            }
            store.set_sync_state(cfg.chain_id, safe, safe)?;
            next_block = Some(safe + 1);
        } else {
            next_block = Some(cursor);
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }

    Ok(indexed_total)
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
/// the report can never show more than the live indexer has been running.
///
/// Unlike `run_live` no reorg sweeps are applied: everything in the range is
/// finalized once it lags `confirmations`, and tip-fork handling stays with
/// the live indexer. Token profit USD is priced historically (Llama keyed by
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
    if to_block < from_block {
        anyhow::bail!("to_block ({to_block}) < from_block ({from_block})");
    }
    let tip = rpc.get_block_number().await.unwrap_or(to_block);
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
    let emit = |done: u64| {
        let mut evt = ProgressEvent::stage("backfill");
        evt.done = Some(done);
        evt.total = Some(total);
        progress.emit(evt);
    };
    emit(0);

    if missing.is_empty() {
        store.set_sync_state(cfg.chain_id, tip, end)?;
        if total > 0 {
            emit(total);
        }
        return Ok(outcome);
    }

    let cap = block_concurrency.max(1).min(missing.len());
    if cap <= 1 {
        for &block in &missing {
            if block.saturating_sub(from_block) % 256 == 0 {
                native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
            }
            let indexed = index_block(rpc, store, cfg, pools, block, native_price, None).await?;
            outcome.blocks_processed += 1;
            outcome.ops += indexed.ops as u64;
            let done = block - from_block + 1;
            if done.is_multiple_of(500) || done == total {
                emit(done);
            }
        }
    } else {
        let sem = Arc::new(Semaphore::new(cap));
        let mut inflight = FuturesUnordered::new();
        let mut ready: BTreeMap<u64, BlockBundle> = BTreeMap::new();
        let mut next_spawn = 0usize;
        let mut next_persist = 0usize;

        let spawn_one =
            |next_spawn: &mut usize, inflight: &mut FuturesUnordered<_>| -> anyhow::Result<()> {
                if *next_spawn >= missing.len() {
                    return Ok(());
                }
                let block = missing[*next_spawn];
                *next_spawn += 1;
                let sem = sem.clone();
                let rpc = rpc.clone();
                inflight.push(async move {
                    let _permit = sem
                        .acquire_owned()
                        .await
                        .map_err(|e| anyhow::anyhow!("backfill fetch semaphore closed: {e}"))?;
                    let bundle = fetch_block_bundle(&rpc, block).await?;
                    Ok::<_, anyhow::Error>((block, bundle))
                });
                Ok(())
            };

        while next_spawn < missing.len() && inflight.len() < cap {
            spawn_one(&mut next_spawn, &mut inflight)?;
        }

        while next_persist < missing.len() {
            let need = missing[next_persist];
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
                    },
                )
                .await?;
                outcome.blocks_processed += 1;
                outcome.ops += indexed.ops as u64;
                next_persist += 1;
                let done = need - from_block + 1;
                if done.is_multiple_of(500) || done == total {
                    emit(done);
                }
                while next_spawn < missing.len() && inflight.len() < cap {
                    spawn_one(&mut next_spawn, &mut inflight)?;
                }
                continue;
            }

            match inflight.next().await {
                Some(Ok((block, bundle))) => {
                    ready.insert(block, bundle);
                    while next_spawn < missing.len() && inflight.len() < cap {
                        spawn_one(&mut next_spawn, &mut inflight)?;
                    }
                }
                Some(Err(e)) => return Err(e),
                None => anyhow::bail!(
                    "backfill fetch pipeline stalled waiting for block {need} \
                     (spawned {next_spawn}/{}, ready {})",
                    missing.len(),
                    ready.len()
                ),
            }
        }
    }

    store.set_sync_state(cfg.chain_id, tip, end)?;
    if total > 0 {
        emit(total);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::MevKind;
    use alloy::primitives::U256;

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
        assert_eq!(avax.liquidation_protocol_aliases.len(), 22);
        assert_eq!(
            avax.liquidation_protocol_aliases
                .get(&address!("5c0401e81bc07ca70fad469b451682c0d747ef1c"))
                .copied(),
            Some("benqi")
        );
        let eth = IngestConfig::from_chain(ChainName::Ethereum, &chains["ethereum"]);
        assert_eq!(
            eth.liquidation_protocol_aliases
                .get(&address!("c13e21b648a5ee794902342038ff3adab66be987"))
                .copied(),
            Some("spark")
        );
        let poly = IngestConfig::from_chain(ChainName::Polygon, &chains["polygon"]);
        assert!(poly.liquidation_protocol_aliases.is_empty());
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
