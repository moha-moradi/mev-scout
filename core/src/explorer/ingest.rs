//! Explorer ingest — block/receipt fetch, decode, classify, persist.
//!
//! Consumes the same `get_block_and_receipts_batch` path the scanner uses,
//! decodes logs into explorer facts, runs the classifier, and persists into
//! the explorer store. Idempotent per block (checkpoints in
//! `blocks_classified`); reorg-aware via stored block hashes; confirmation
//! lag applied before indexing.

use std::collections::HashMap;

use alloy::primitives::{Address, B256};
use tracing::{debug, warn};

use crate::explorer::classify::{self, BlockInput, TxInput};
use crate::explorer::pricing::{self, TokenUsd};
use crate::explorer::store::{
    BlockFactsInput, ExplorerStore, OpenPosition, SwapRow, TransferRow, TxRow,
};
use crate::explorer::types::{MevEvent, MevKind};
use crate::progress::{JobProgress, ProgressEvent};
use crate::rpc::RpcClient;
use crate::types::ChainName;

/// JIT open-position window (Phase 1.5): positions opened more than this many
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
    /// Mevlive-parity fallback for `arb_atomic` (Phase 1.2). Default false —
    /// only closed multi-pool cycles are labeled arb (spec §7.1 / §8.1).
    pub arb_likely_parity: bool,
}

impl IngestConfig {
    pub fn from_chain(chain: ChainName, chain_config: &crate::config::ChainConfig) -> Self {
        let wrapped_native = chain_config.wrapped_native_token.unwrap_or(Address::ZERO);
        let priority = build_profit_priority(chain, wrapped_native);
        IngestConfig {
            chain,
            chain_id: chain.chain_id(),
            confirmations: 6,
            wrapped_native,
            profit_token_priority: priority,
            arb_likely_parity: false,
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

/// Fetch one block + receipts, decode, classify, persist. Idempotent: an
/// already-classified block is skipped. When `token_prices` is `None`, USD
/// prices for the block's profit tokens are warmed on demand.
pub async fn index_block(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pool_tokens: &HashMap<Address, (Address, Address)>,
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

    let (block_data, txs, receipts) = rpc.get_block_and_receipts_batch(block_number).await?;

    // Build per-tx decoded input.
    let receipt_by_index: HashMap<u64, &crate::data::ReceiptData> =
        receipts.iter().map(|r| (r.tx_index, r)).collect();

    let mut tx_inputs: Vec<TxInput> = Vec::with_capacity(txs.len());
    let mut tx_rows: Vec<TxRow> = Vec::with_capacity(txs.len());
    let mut swap_rows: Vec<SwapRow> = Vec::new();
    let mut transfer_rows: Vec<TransferRow> = Vec::new();
    let mut jit_hashes: HashMap<u64, B256> = HashMap::new();

    let base_fee_gwei = block_data.base_fee_per_gas.map(|b| b as f64 / 1e9);

    for tx in &txs {
        let receipt = match receipt_by_index.get(&tx.index) {
            Some(r) => *r,
            None => continue,
        };
        if !receipt.status {
            continue; // failed txs carry no realized MEV
        }

        let (transfers, swaps, liquidations, flashloans, jit) =
            classify::decode_tx_logs(tx.index, &receipt.logs, pool_tokens);
        let jit_count = jit.len();

        // Real gas (Phase 2.1): prefer the receipt's `effectiveGasPrice`, then
        // the tx's legacy `gasPrice`, and only fall back to
        // `base_fee + max_priority_fee` when the node omitted both.
        let max_priority_gwei = tx
            .max_priority_fee_per_gas
            .map(|p| p as f64 / 1e9)
            .unwrap_or(0.0);
        let effective_gwei = receipt
            .effective_gas_price
            .or(tx.gas_price)
            .map(|p| p as f64 / 1e9)
            .unwrap_or_else(|| base_fee_gwei.unwrap_or(0.0).max(0.0) + max_priority_gwei);
        let priority_gwei = if tx.max_priority_fee_per_gas.is_some() {
            max_priority_gwei.min(effective_gwei)
        } else {
            (effective_gwei - base_fee_gwei.unwrap_or(0.0)).max(0.0)
        };

        tx_rows.push(TxRow {
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
            swap_rows.push(SwapRow {
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
            transfer_rows.push(TransferRow {
                tx_index: t.tx_index,
                log_index: t.log_index,
                token: t.token,
                from: t.from,
                to: t.to,
                amount: t.amount,
            });
        }
        if jit_count > 0 {
            jit_hashes.insert(tx.index, tx.hash);
        }

        tx_inputs.push(TxInput {
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
            jit,
        });
    }

    let open_positions = store.open_positions(block_number.saturating_sub(JIT_WINDOW_BLOCKS))?;
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
        txs: tx_inputs,
    };

    let mut events: Vec<MevEvent> = classify::filter_unresolved(classify::classify_block(&input));
    classify::stamp_jit_tx_hashes(&mut events, &jit_hashes);
    let ops = events.len();

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

/// Persist JIT open positions (Phase 1.5): record Mints not closed in the same
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

/// Cheap per-poll reorg re-verify (Phase 4): a single header-only hash compare
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
    // ERC-20 decimals() via Multicall3 (Phase 2.4 long-tail fallback).
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
    // Liquidation P&L needs the repaid debt asset priced too (Phase 1.3).
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
/// hash-verified on every poll (Phase 4) and a heavier reorg sweep runs every
/// 32 blocks.
pub async fn run_live(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    pool_tokens: &HashMap<Address, (Address, Address)>,
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
            // Phase 4: cheap per-poll reorg re-verify on the last indexed block.
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
                    index_block(rpc, store, cfg, pool_tokens, block, native_price, None).await?;
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
    pool_tokens: &HashMap<Address, (Address, Address)>,
    from_block: u64,
    to_block: u64,
    progress: &dyn JobProgress,
) -> anyhow::Result<RangeOutcome> {
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
    let mut done: u64 = 0;
    let mut outcome = RangeOutcome::default();
    let mut native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
    let emit = |done: u64| {
        let mut evt = ProgressEvent::stage("backfill");
        evt.done = Some(done);
        evt.total = Some(total);
        progress.emit(evt);
    };
    emit(0);
    for block in from_block..=end {
        done += 1;
        if block.saturating_sub(from_block) % 256 == 0 {
            native_price = native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
        }
        if store.block_classified(block)? {
            continue;
        }
        let indexed = index_block(rpc, store, cfg, pool_tokens, block, native_price, None).await?;
        outcome.blocks_processed += 1;
        outcome.ops += indexed.ops as u64;
        if done.is_multiple_of(500) || done == total {
            emit(done);
        }
    }
    store.set_sync_state(cfg.chain_id, tip, end)?;
    if total > 0 && !done.is_multiple_of(500) {
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
        assert!(ic.profit_token_priority.len() >= 4);
        assert!(!ic.wrapped_native.is_zero());
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
