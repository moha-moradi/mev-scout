//! Replay a specific cached block for debugging.

use std::collections::HashMap;
use std::time::Instant;

use alloy::primitives::{keccak256, Address};
use anyhow::Context;

use crate::cache::SqliteStore;
use crate::config::validation;
use crate::config::Config;
use crate::pool::state::PoolInfo;
use crate::progress::JobProgress;
use crate::replay::BlockReplayer;

use super::rpc::init_rpc;

#[derive(Debug, Clone)]
pub struct ReplayOpts {
    pub block: u64,
    pub tx_index: Option<usize>,
    pub analyze: bool,
}

pub struct ReplayTxRow {
    pub index: u64,
    pub tx_hash: String,
    pub status: bool,
    pub gas_used: u64,
    pub receipt_ok: bool,
    pub dex_interactions: Vec<String>,
}

pub struct ReplayOutcome {
    pub block: u64,
    pub chain: String,
    pub tx_count: usize,
    pub end_tx: usize,
    pub matched: u64,
    pub total: u64,
    pub match_pct: f64,
    pub elapsed_secs: f64,
    pub rows: Vec<ReplayTxRow>,
}

pub async fn job_replay(
    config: &Config,
    opts: &ReplayOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ReplayOutcome> {
    let (chain_name, chain_config) =
        validation::resolve_chain(config).context("failed to resolve chain")?;
    if opts.block == 0 {
        anyhow::bail!("--block is required and must be > 0");
    }

    let setup = init_rpc(config, chain_name, true).await?;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;

    let block_num = opts.block;
    let tx_index = opts.tx_index.unwrap_or(usize::MAX);

    if !cache.has_block(block_num)? {
        anyhow::bail!(
            "block {block_num} is not cached (run fetch --block {block_num} first)"
        );
    }

    let pool_map: HashMap<Address, PoolInfo> = if opts.analyze {
        let mut map = HashMap::new();
        if let Ok(pools) = cache.list_discovered_pools() {
            for pool in pools {
                map.insert(pool.address, pool);
            }
        }
        progress.log(&format!("Loaded {} pools for DEX analysis", map.len()));
        map
    } else {
        HashMap::new()
    };

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache,
        rpc,
        chain_config.chain_id,
    );
    let txs = replayer
        .load_txs(block_num)
        .with_context(|| format!("Failed to load txs for block {block_num}"))?;
    let actual_count = txs.len();
    let end_tx = tx_index.min(actual_count.saturating_sub(1));

    progress.log(&format!(
        "Replaying block {block_num} on chain {chain_name} ({actual_count} txs, replaying 0..{end_tx})"
    ));

    let start = Instant::now();
    let (_snapshot, results) = replayer
        .replay_to(block_num, end_tx)
        .with_context(|| format!("Replay failed for block {block_num}"))?;
    let elapsed = start.elapsed();

    let t_swap_v2 = keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)");
    let t_sync = keccak256(b"Sync(uint112,uint112)");
    let t_swap_v3 = keccak256(b"Swap(address,address,int256,int256,uint160,uint128,int24)");
    let t_mint = keccak256(b"Mint(address,address,int24,int24,uint128,uint256,uint256)");
    let t_burn = keccak256(b"Burn(address,address,int24,int24,uint128,uint256,uint256)");

    let mut matched = 0u64;
    let mut total = 0u64;
    let mut rows = Vec::new();

    for r in &results {
        let receipt_ok = r.error.is_none();
        if receipt_ok {
            matched += 1;
        }
        total += 1;

        let mut dex_interactions = Vec::new();
        if opts.analyze {
            for log in &r.logs {
                if let Some(info) = pool_map.get(&log.address) {
                    let event_type = if log.topics.is_empty() {
                        "Unknown"
                    } else {
                        match log.topics[0] {
                            x if x == t_swap_v2 || x == t_swap_v3 => "Swap",
                            x if x == t_sync => "Sync",
                            x if x == t_mint => "Mint",
                            x if x == t_burn => "Burn",
                            _ => "Unknown",
                        }
                    };
                    let name = info
                        .name
                        .as_deref()
                        .map(String::from)
                        .unwrap_or_else(|| info.address.to_string());
                    dex_interactions.push(format!("{name} — {event_type}"));
                }
            }
        }

        progress.log(&format!(
            "  {:>4} {} {} {} {}",
            r.index,
            r.tx_hash,
            if r.status { "ok" } else { "fail" },
            r.gas_used,
            if receipt_ok { "✓" } else { "✗" },
        ));
        for (j, line) in dex_interactions.iter().enumerate() {
            let prefix = if j + 1 == dex_interactions.len() {
                "         └ "
            } else {
                "         ├ "
            };
            progress.log(&format!("{prefix}{line}"));
        }
        if opts.analyze && dex_interactions.is_empty() {
            progress.log("         (no DEX interactions)");
        }

        rows.push(ReplayTxRow {
            index: r.index,
            tx_hash: r.tx_hash.to_string(),
            status: r.status,
            gas_used: r.gas_used,
            receipt_ok,
            dex_interactions,
        });
    }

    let match_pct = if total > 0 {
        (matched as f64 / total as f64) * 100.0
    } else {
        100.0
    };
    progress.log(&format!(
        "  Receipt verification: {matched}/{total} match ({match_pct:.1}%) — {:.2}s",
        elapsed.as_secs_f64()
    ));
    if match_pct < 99.0 {
        tracing::warn!("Receipt match rate {match_pct:.1}% is below 99% threshold");
    }

    Ok(ReplayOutcome {
        block: block_num,
        chain: chain_name.to_string(),
        tx_count: actual_count,
        end_tx,
        matched,
        total,
        match_pct,
        elapsed_secs: elapsed.as_secs_f64(),
        rows,
    })
}
