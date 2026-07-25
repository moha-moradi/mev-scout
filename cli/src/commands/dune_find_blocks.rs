use crate::cli::DuneFindBlocksArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::DuneClient;

/// Approximate the minimum block_month date for Dune partition pruning.
fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let (genesis_ts, secs_per_block) = match chain {
        "ethereum" => (1438269988_i64, 12.0),
        "polygon" => (1591031691, 2.1),
        "bsc" => (1597734000, 3.0),
        "avalanche_c" | "avalanche" => (1624402800, 2.0),
        "arbitrum" => (1630812600, 0.26),
        "base" => (1686787200, 2.0),
        "optimism" => (1631808000, 2.0),
        _ => (1609459200, 12.0),
    };
    let elapsed = block_number as f64 * secs_per_block;
    let approx_ts = genesis_ts + elapsed as i64;
    let naive = chrono::DateTime::from_timestamp(approx_ts, 0)
        .unwrap_or_default();
    naive.format("%Y-%m-%d").to_string()
}

/// Map chain name to Dune chain label.
fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}

fn estimate_blocks_per_day(chain: &str) -> u64 {
    match chain {
        "ethereum" => 7200,
        "polygon" => 41000,
        "bsc" => 28800,
        "avalanche" | "avalanche_c" => 43200,
        "arbitrum" => 330000,
        "base" => 43200,
        "optimism" => 43200,
        _ => 7200,
    }
}

/// Estimate the latest block number from current time (reverse of approx_block_month_min).
fn estimate_latest_block(chain: &str) -> u64 {
    let (genesis_ts, secs_per_block) = match chain {
        "ethereum" => (1438269988_i64, 12.0),
        "polygon" => (1591031691, 2.1),
        "bsc" => (1597734000, 3.0),
        "avalanche_c" | "avalanche" => (1624402800, 2.0),
        "arbitrum" => (1630812600, 0.26),
        "base" => (1686787200, 2.0),
        "optimism" => (1631808000, 2.0),
        _ => (1609459200, 12.0),
    };
    let now = chrono::Utc::now().timestamp();
    let elapsed_secs = now - genesis_ts;
    (elapsed_secs as f64 / secs_per_block) as u64
}

/// Dune indexing lag in blocks (conservative estimate per chain).
/// Dune data pipelines typically lag well behind chain head on free tier.
fn dune_indexing_lag(chain: &str) -> u64 {
    let lag_secs = match chain {
        "ethereum" => 60 * 24 * 3600,     // ~60 days
        "polygon" => 60 * 24 * 3600,
        "bsc" => 60 * 24 * 3600,
        "avalanche_c" | "avalanche" => 60 * 24 * 3600,
        "arbitrum" => 60 * 24 * 3600,
        "base" => 60 * 24 * 3600,
        "optimism" => 60 * 24 * 3600,
        _ => 60 * 24 * 3600,
    };
    let secs_per_block = match chain {
        "ethereum" => 12.0,
        "polygon" => 2.1,
        "bsc" => 3.0,
        "avalanche_c" | "avalanche" => 2.0,
        "arbitrum" => 0.26,
        "base" => 2.0,
        "optimism" => 2.0,
        _ => 12.0,
    };
    (lag_secs as f64 / secs_per_block) as u64
}

pub async fn cmd_dune_find_blocks(
    config: &Config,
    args: &DuneFindBlocksArgs,
) -> anyhow::Result<()> {
    let api_key = args
        .dune_api_key
        .clone()
        .or_else(|| config.dune_api_key.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No Dune API key found. Set it in mev-scout.toml (dune_api_key = \"...\") or pass --dune-api-key"
            )
        })?;

    let client = DuneClient::new(api_key);
    let chain = &args.chain;
    let chain_label = dune_chain_label(chain);

    let blocks_per_day = estimate_blocks_per_day(chain);
    let range_blocks = args.days * blocks_per_day;

    let to_block = args.to_block.unwrap_or_else(|| {
        let latest = estimate_latest_block(&chain_label);
        let lag = dune_indexing_lag(&chain_label);
        latest.saturating_sub(lag)
    });
    let from_block = to_block.saturating_sub(range_blocks);

    let block_month_min = if from_block > 0 {
        approx_block_month_min(from_block, &chain_label)
    } else {
        "2024-01-01".to_string()
    };

    let find_arbs = args.mev_type == "both" || args.mev_type == "arbitrage";
    let find_sandwiches = args.mev_type == "both" || args.mev_type == "sandwich";

    let mut block_scores: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();

    if find_arbs {
        let sql = format!(
            r#"WITH tx_pools AS (
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address AS pool_address,
    COUNT(*) OVER (PARTITION BY t.block_number, t.tx_hash) AS pool_count
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
)
SELECT block_number, COUNT(DISTINCT tx_hash) AS arb_count
FROM tx_pools
WHERE pool_count >= 2
GROUP BY block_number
ORDER BY arb_count DESC
LIMIT {limit}"#,
            chain = chain_label,
            block_month_min = block_month_min,
            from_block = from_block,
            to_block = to_block,
            limit = args.top * 3,
        );

        eprintln!(
            "Querying Dune for arbitrage blocks on {} (blocks {}–{})...",
            chain, from_block, to_block
        );

        match client.execute_raw_sql(&sql).await {
            Ok(result) => {
                if let Some(ref r) = result.result {
                    for row in &r.rows {
                        let block = row
                            .get("block_number")
                            .and_then(|v| {
                                if let Some(n) = v.as_u64() {
                                    return Some(n);
                                }
                                v.as_str().and_then(|s| s.parse::<u64>().ok())
                            })
                            .unwrap_or(0);
                        let count = row
                            .get("arb_count")
                            .and_then(|v| {
                                if let Some(n) = v.as_u64() {
                                    return Some(n);
                                }
                                v.as_str().and_then(|s| s.parse::<u64>().ok())
                            })
                            .unwrap_or(0);
                        if block > 0 {
                            *block_scores.entry(block).or_insert(0) += count;
                        }
                    }
                    eprintln!("  Found {} blocks with arbitrages", r.rows.len());
                }
            }
            Err(e) => {
                eprintln!("  Arbitrage query failed: {}", e);
            }
        }
    }

    // Avoid rate limits on Dune free tier
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    if find_sandwiches {
        let sql = format!(
            r#"SELECT block_number, COUNT(*) AS sandwich_count
FROM dex.sandwiches
WHERE blockchain = '{chain}'
  AND block_month >= DATE '{block_month_min}'
  AND block_number >= {from_block}
  AND block_number <= {to_block}
GROUP BY block_number
ORDER BY sandwich_count DESC
LIMIT {limit}"#,
            chain = chain_label,
            block_month_min = block_month_min,
            from_block = from_block,
            to_block = to_block,
            limit = args.top * 3,
        );

        eprintln!(
            "Querying Dune for sandwich blocks on {} (blocks {}–{})...",
            chain, from_block, to_block
        );

        match client.execute_raw_sql(&sql).await {
            Ok(result) => {
                if let Some(ref r) = result.result {
                    for row in &r.rows {
                        let block = row
                            .get("block_number")
                            .and_then(|v| {
                                if let Some(n) = v.as_u64() {
                                    return Some(n);
                                }
                                v.as_str().and_then(|s| s.parse::<u64>().ok())
                            })
                            .unwrap_or(0);
                        let count = row
                            .get("sandwich_count")
                            .and_then(|v| {
                                if let Some(n) = v.as_u64() {
                                    return Some(n);
                                }
                                v.as_str().and_then(|s| s.parse::<u64>().ok())
                            })
                            .unwrap_or(0);
                        if block > 0 {
                            *block_scores.entry(block).or_insert(0) += count;
                        }
                    }
                    eprintln!("  Found {} blocks with sandwiches", r.rows.len());
                }
            }
            Err(e) => {
                eprintln!("  Sandwich query failed: {}", e);
            }
        }
    }

    let mut sorted: Vec<(u64, u64)> = block_scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    let top: Vec<u64> = sorted.into_iter().take(args.top).map(|(b, _)| b).collect();

    if top.is_empty() {
        eprintln!("\nNo candidate blocks found.");
        eprintln!("Check your Dune API key, chain name, and block range.");
    } else {
        eprintln!("\nTop {} candidate blocks:", top.len());
        for block in &top {
            println!("{}", block);
        }
    }

    Ok(())
}
