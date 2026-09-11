//! Explorer ingest — block/receipt fetch, decode, classify, persist (§7).
//!
//! Consumes the same `get_block_and_receipts_batch` path the scanner uses,
//! decodes logs into explorer facts, runs the classifier, and persists into
//! the explorer store. Idempotent per block (checkpoints in
//! `blocks_classified`); reorg-aware via stored block hashes; confirmation
//! lag applied before indexing (§5.2).

use std::collections::HashMap;

use alloy::primitives::{Address, B256};
use tracing::{debug, warn};

use crate::explorer::classify::{self, BlockInput, TxInput};
use crate::explorer::pricing::{self, TokenUsd};
use crate::explorer::store::{ExplorerStore, SwapRow, TransferRow, TxRow};
use crate::explorer::types::MevEvent;
use crate::rpc::RpcClient;
use crate::types::ChainName;

/// Ingester configuration.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub chain: ChainName,
    pub chain_id: u64,
    /// Blocks of lag behind head before indexing (§5.2: default 6 ≈ 12s on Polygon).
    pub confirmations: u64,
    /// Wrapped-native token from chain config (wrap-noise filter).
    pub wrapped_native: Address,
    /// Profit-token priority from chain config (§8.2.4).
    pub profit_token_priority: Vec<Address>,
}

impl IngestConfig {
    pub fn from_chain(chain: ChainName, chain_config: &crate::config::ChainConfig) -> Self {
        let wrapped_native = chain_config
            .wrapped_native_token
            .as_deref()
            .and_then(|s| s.parse::<Address>().ok())
            .unwrap_or(Address::ZERO);
        let priority = build_profit_priority(chain, wrapped_native);
        IngestConfig {
            chain,
            chain_id: chain.chain_id(),
            confirmations: 6,
            wrapped_native,
            profit_token_priority: priority,
        }
    }
}

/// Profit-token priority: USDC → USDT → DAI → wrapped native (§8.2.4).
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

/// Fetch one block + receipts, decode, classify, persist. Idempotent: an
/// already-classified block is skipped. When `token_prices` is `None`, USD
/// prices for the block's profit tokens are warmed on demand.
pub async fn index_block(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    block_number: u64,
    native_price: Option<f64>,
    token_prices: Option<HashMap<Address, TokenUsd>>,
) -> anyhow::Result<IndexedBlock> {
    if store.block_classified(block_number)? {
        debug!(block = block_number, "already classified, skipping");
        return Ok(IndexedBlock { block: block_number, events: 0, ops: 0 });
    }

    let (block_data, txs, receipts) = rpc.get_block_and_receipts_batch(block_number).await?;

    // Build per-tx decoded input.
    let receipt_by_index: HashMap<u64, &crate::data::ReceiptData> = receipts
        .iter()
        .map(|r| (r.tx_index, r))
        .collect();

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

        let (transfers, swaps, liquidations, jit) = classify::decode_tx_logs(tx.index, &receipt.logs);
        let jit_count = jit.len();

        // Effective gas price: receipt has gasUsed; effective = base + priority.
        // The scanner's receipt conversion does not carry effectiveGasPrice, so
        // approximate as base_fee + max_priority when positive, else base_fee.
        let max_priority_gwei = tx
            .max_priority_fee_per_gas
            .map(|p| p as f64 / 1e9)
            .unwrap_or(0.0);
        let effective_gwei = base_fee_gwei.unwrap_or(0.0).max(0.0) + max_priority_gwei;
        let priority_gwei = max_priority_gwei.min(effective_gwei);

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
            jit,
        });
    }

    let input = BlockInput {
        block: block_number,
        ts: block_data.timestamp,
        wrapped_native: cfg.wrapped_native,
        profit_policy: crate::explorer::profit::ProfitTokenPolicy {
            priority: cfg.profit_token_priority.clone(),
            wrapped_native: cfg.wrapped_native,
            weth: cfg.wrapped_native,
        },
        txs: tx_inputs,
    };

    let mut events: Vec<MevEvent> = classify::classify_block(&input);
    classify::stamp_jit_tx_hashes(&mut events, &jit_hashes);
    let ops = events.len();

    let token_prices = match token_prices {
        Some(p) => p,
        None => {
            let tokens = event_tokens(&events);
            warm_prices_for_tokens(cfg, &tokens, block_data.timestamp, store).await?
        }
    };

    store.insert_block_facts(
        block_number,
        &block_data.hash,
        block_data.timestamp,
        base_fee_gwei,
        txs.len(),
        &tx_rows,
        &swap_rows,
        &transfer_rows,
        &events,
        native_price,
        &token_prices,
    )?;

    Ok(IndexedBlock { block: block_number, events: txs.len(), ops })
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

/// Outcome of a stats-window price fill.
pub async fn warm_prices_for_tokens(
    cfg: &IngestConfig,
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
            .and_then(|(_, d)| d.map(|d| u32::try_from(d).unwrap_or(18)))
    };

    let hour = pricing::hour_bucket(ts);
    let mut missing: Vec<Address> = Vec::new();
    for t in tokens {
        if t.is_zero() || *t == crate::explorer::profit::NATIVE_MARKER {
            continue;
        }
        if let Some((usd, _)) = store.price_at(*t, hour)? {
            if let Some(dec) = decimals_of(t) {
                out.insert(*t, TokenUsd { usd, decimals: dec });
                continue;
            }
        }
        missing.push(*t);
    }
    if missing.is_empty() {
        return Ok(out);
    }

    match pricing::fetch_prices_llama(cfg.chain, ts, &missing).await {
        Ok(prices) => {
            for (addr, (usd, llama_dec)) in prices {
                let dec = llama_dec.or_else(|| decimals_of(&addr));
                if let Some(dec) = dec {
                    store.put_price(addr, hour, usd, "llama")?;
                    out.insert(addr, TokenUsd { usd, decimals: dec });
                }
            }
        }
        Err(e) => warn!("llama price fetch failed ({} tokens): {e}", missing.len()),
    }
    Ok(out)
}

/// Collect all token addresses appearing in events (for price warming).
pub fn event_tokens(events: &[MevEvent]) -> Vec<Address> {
    let mut v: Vec<Address> = events
        .iter()
        .filter_map(|e| e.profit_token)
        .filter(|t| !t.is_zero() && *t != crate::explorer::profit::NATIVE_MARKER)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Native-token USD price with hourly store cache (CoinGecko live path).
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
    match pricing::fetch_native_price_coingecko(cfg.chain).await {
        Ok(usd) => {
            store.put_price(marker, hour, usd, "coingecko")?;
            Ok(Some(usd))
        }
        Err(e) => {
            warn!("native price fetch failed: {e}");
            Ok(None)
        }
    }
}

/// Consecutive-range backfill worker: indexes `[from, to]` in order with
/// per-block idempotency and periodic sync checkpointing. Returns ops indexed.
pub async fn backfill_range(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    from: u64,
    to: u64,
    checkpoint_every: u64,
) -> anyhow::Result<(u64, u64)> {
    let ts_now = crate::utils::epoch_secs();
    let native_price = native_price_cached(cfg, store, ts_now).await?;
    let mut ops_total: u64 = 0;
    let mut blocks_done: u64 = 0;
    let mut since_checkpoint: u64 = 0;

    for block in from..=to {
        let indexed = index_block(rpc, store, cfg, block, native_price, None).await?;
        ops_total += indexed.ops as u64;
        blocks_done += 1;
        since_checkpoint += 1;
        if since_checkpoint >= checkpoint_every {
            store.set_sync_state(cfg.chain_id, to, block)?;
            since_checkpoint = 0;
        }
    }
    store.set_sync_state(cfg.chain_id, to, to)?;
    Ok((blocks_done, ops_total))
}

/// Live streaming mode: follow head − confirmations, indexing each new block.
/// Runs until `stop` is set; reorg-checked every 32 blocks.
pub async fn run_live(
    rpc: &RpcClient,
    store: &ExplorerStore,
    cfg: &IngestConfig,
    poll_ms: u64,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<u64> {
    use std::sync::atomic::Ordering;

    let mut indexed_total: u64 = 0;
    let mut since_reorg_check: u64 = 0;
    let mut next_block = store.get_indexed_to(cfg.chain_id)?.saturating_add(1);

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

        if safe >= next_block {
            // Reorg check on the previous indexed block before continuing.
            since_reorg_check += 1;
            if since_reorg_check >= 32 {
                let check_at = next_block.saturating_sub(1);
                if check_reorg(rpc, store, check_at).await?.is_some() {
                    next_block = check_at; // re-index from fork
                }
                since_reorg_check = 0;
            }
            let native_price =
                native_price_cached(cfg, store, crate::utils::epoch_secs()).await?;
            for block in next_block..=safe {
                let indexed = index_block(rpc, store, cfg, block, native_price, None).await?;
                indexed_total += indexed.ops as u64;
            }
            store.set_sync_state(cfg.chain_id, safe, safe)?;
            next_block = safe + 1;
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }

    Ok(indexed_total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;
    use crate::explorer::types::MevKind;

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
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            confidence: crate::explorer::types::Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({}),
        }
    }
}
