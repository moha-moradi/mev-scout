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
    let approx_epoch = genesis_ts + elapsed as i64;

    let days = approx_epoch / 86400;
    let era = (days >= 0).then_some(days).unwrap_or(days - 146096) / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (mp * 153 + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}", y, m, d)
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

    let to_block = args.to_block.unwrap_or(0);
    let (from_block, to_block) = if to_block > 0 {
        (to_block.saturating_sub(range_blocks), to_block)
    } else {
        (0, 0)
    };

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
    t.pool_address,
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
