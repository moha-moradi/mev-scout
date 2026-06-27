use std::collections::HashMap;

use alloy::primitives::{keccak256, Address};

use crate::cli::ReplayArgs;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::rpc::RpcClient;

pub async fn cmd_replay(config: &Config, args: &ReplayArgs) -> anyhow::Result<()> {
    let (chain_name, chain_config) = match validation::validate_replay(config) {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{}", e),
    };

    let provider_configs = config.effective_provider_configs(chain_name)?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_config.chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
    rpc.check_connection(chain_config.chain_id).await?;
    let cache = SqliteStore::open(&config.db_path, chain_config.chain_id)?;

    let block_num = args.block;
    let tx_index = args.tx_index.unwrap_or(usize::MAX);

    if !cache.has_block(block_num)? {
        anyhow::bail!(
            "Error: Block {} is not cached. Run `mev-scout fetch --block {}` first.",
            block_num, block_num
        );
    }

    let pool_map: HashMap<Address, PoolInfo> = if args.analyze {
        let mut map = HashMap::new();
        if let Ok(pools) = cache.list_discovered_pools() {
            for pool in pools {
                map.insert(pool.address, pool);
            }
        }
        tracing::info!("Loaded {} pools for DEX analysis", map.len());
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
        .map_err(|e| anyhow::anyhow!("Failed to load txs for block {}: {}", block_num, e))?;
    let actual_count = txs.len();
    let end_tx = tx_index.min(actual_count.saturating_sub(1));

    println!(
        "Replaying block {} on chain {} ({} txs, replaying 0..{})",
        block_num, chain_name, actual_count, end_tx
    );
    println!();

    let start = std::time::Instant::now();
    let (_snapshot, results) = replayer
        .replay_to(block_num, end_tx)
        .map_err(|e| anyhow::anyhow!("Replay failed for block {}: {}", block_num, e))?;
    let elapsed = start.elapsed();

    println!(
        "  {:<4} {:<66} {:<6} {:<8} receipt",
        "idx", "tx_hash", "status", "gas_used",
    );
    println!("  {}", "─".repeat(100));

    let mut matched = 0u64;
    let mut total = 0u64;

    for r in &results {
        let status_str = if r.status { "ok" } else { "fail" };
        let receipt_str = match &r.error {
            None => {
                matched += 1;
                "✓".to_string()
            }
            Some(_) => "✗".to_string(),
        };
        total += 1;

        println!(
            "  {:<4} {:<66} {:<6} {:<8} {}",
            r.index, r.tx_hash, status_str, r.gas_used, receipt_str
        );

        if args.analyze {
            let interactions: Vec<String> = r.logs.iter().filter_map(|log| {
                pool_map.get(&log.address).map(|info| {
                    let event_type = if log.topics.is_empty() {
                        "Unknown"
                    } else {
                        let t0 = log.topics[0];
                        if t0 == keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)") {
                            "Swap"
                        } else if t0 == keccak256(b"Sync(uint112,uint112)") {
                            "Sync"
                        } else if t0 == keccak256(b"Swap(address,address,int256,int256,uint160,uint128,int24)") {
                            "Swap"
                        } else if t0 == keccak256(b"Mint(address,address,int24,int24,uint128,uint256,uint256)") {
                            "Mint"
                        } else if t0 == keccak256(b"Burn(address,address,int24,int24,uint128,uint256,uint256)") {
                            "Burn"
                        } else {
                            "Unknown"
                        }
                    };
                    let name = info.name.clone().unwrap_or_else(|| format!("{}", info.address));
                    format!("{} — {}", name, event_type)
                })
            }).collect();

            if interactions.is_empty() {
                println!("         (no DEX interactions)");
            } else {
                println!("         DEX interactions:");
                for (j, line) in interactions.iter().enumerate() {
                    let prefix = if j == interactions.len() - 1 { "         └ " } else { "         ├ " };
                    println!("{}{}", prefix, line);
                }
            }
        }
    }

    println!();
    let pct = if total > 0 {
        (matched as f64 / total as f64) * 100.0
    } else {
        100.0
    };
    println!(
        "  Receipt verification: {}/{} match ({:.1}%) — {:.2}s",
        matched, total, pct, elapsed.as_secs_f64()
    );

    if pct < 99.0 {
        tracing::warn!(
            "Receipt match rate {:.1}% is below 99% threshold",
            pct
        );
    }

    Ok(())
}
