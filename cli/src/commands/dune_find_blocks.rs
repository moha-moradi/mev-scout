use crate::cli::DuneFindBlocksArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::DuneClient;
use mev_scout_core::dune::util::{
    approx_block_month_min, chain_timing, dune_chain_label, dune_indexing_lag_blocks,
    estimate_latest_block,
};

pub async fn cmd_dune_find_blocks(
    config: &Config,
    args: &DuneFindBlocksArgs,
) -> anyhow::Result<()> {
    let api_key = args
        .dune_api_key
        .clone()
        .or_else(|| config.dune.dune_api_key.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No Dune API key found. Set it in mev-scout.toml (dune_api_key = \"...\") or pass --dune-api-key"
            )
        })?;

    let client = DuneClient::new(api_key);
    let chain = &args.chain;
    let chain_label = dune_chain_label(chain);

    let blocks_per_day = chain_timing(&chain_label).blocks_per_day;
    let range_blocks = args.days * blocks_per_day;

    let to_block = args.to_block.unwrap_or_else(|| {
        let latest = estimate_latest_block(&chain_label);
        let lag = dune_indexing_lag_blocks(&chain_label);
        latest.saturating_sub(lag)
    });
    let from_block = to_block.saturating_sub(range_blocks);

    let block_month_min = if from_block > 0 {
        approx_block_month_min(from_block, &chain_label)
    } else {
        "2024-01-01".to_string()
    };

    let mev_type = args.mev_type.to_lowercase();
    let find_arbs = mev_type == "all" || mev_type == "arbitrage" || mev_type == "both";
    let find_sandwiches = mev_type == "all" || mev_type == "sandwich" || mev_type == "both";
    let find_jit = mev_type == "all" || mev_type == "jit";
    let find_liquidations = mev_type == "all" || mev_type == "liquidation";
    let find_flash_loans = mev_type == "all" || mev_type == "flash_loan";

    eprintln!(
        "Searching for '{}' MEV blocks on {} (blocks {}–{})...",
        args.mev_type, chain, from_block, to_block
    );

    let mut block_scores: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();

    /// Helper: extract (block_number, count) from a Dune result row.
    fn parse_block_count(
        row: &serde_json::Map<String, serde_json::Value>,
        block_key: &str,
        count_key: &str,
    ) -> Option<(u64, u64)> {
        let block = row.get(block_key).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })?;
        let count = row.get(count_key).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })?;
        Some((block, count))
    }

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
                        if let Some((block, count)) = parse_block_count(row, "block_number", "arb_count") {
                            if block > 0 {
                                *block_scores.entry(block).or_insert(0) += count;
                            }
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
                        if let Some((block, count)) = parse_block_count(row, "block_number", "sandwich_count") {
                            if block > 0 {
                                *block_scores.entry(block).or_insert(0) += count;
                            }
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

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    if find_jit {
        // Polygon: uniswap_v3_polygon decode stopped at 2022-09; use the live
        // QuickSwap V3 (Algebra) decode and the quickswap v3 dex.trades label.
        let sql = format!(
            r#"WITH v3_events AS (
  SELECT
    evt_block_number AS block_number,
    evt_tx_hash AS tx_hash,
    contract_address AS pool_address,
    'mint' AS event_type
  FROM quickswap_v3_polygon.algebrapool_evt_mint
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    contract_address,
    'burn'
  FROM quickswap_v3_polygon.algebrapool_evt_burn
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address,
    'swap'
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.project = 'quickswap'
    AND t.version = '3'
),
pool_tx_events AS (
  SELECT
    pool_address,
    block_number,
    tx_hash,
    ARRAY_AGG(DISTINCT event_type) AS event_types,
    COUNT(DISTINCT event_type) AS event_count
  FROM v3_events
  GROUP BY pool_address, block_number, tx_hash
)
SELECT block_number, COUNT(*) AS jit_count
FROM pool_tx_events
WHERE event_count >= 2
  AND contains(event_types, 'mint')
GROUP BY block_number
ORDER BY jit_count DESC
LIMIT {limit}"#,
            chain = chain_label,
            block_month_min = block_month_min,
            from_block = from_block,
            to_block = to_block,
            limit = args.top * 3,
        );

        eprintln!(
            "Querying Dune for JIT liquidity blocks on {} (blocks {}–{})...",
            chain, from_block, to_block
        );

        match client.execute_raw_sql(&sql).await {
            Ok(result) => {
                if let Some(ref r) = result.result {
                    for row in &r.rows {
                        if let Some((block, count)) = parse_block_count(row, "block_number", "jit_count") {
                            if block > 0 {
                                *block_scores.entry(block).or_insert(0) += count;
                            }
                        }
                    }
                    eprintln!("  Found {} blocks with JIT liquidity", r.rows.len());
                }
            }
            Err(e) => {
                eprintln!("  JIT query failed (decoded V3 event tables may not exist for this chain): {}", e);
            }
        }
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    if find_liquidations {
        let sql = format!(
            r#"SELECT block_number, COUNT(*) AS liq_count
FROM lending.borrow
WHERE blockchain = '{chain}'
  AND transaction_type = 'borrow_liquidation'
  AND block_month >= DATE '{block_month_min}'
  AND block_number >= {from_block}
  AND block_number <= {to_block}
GROUP BY block_number
ORDER BY liq_count DESC
LIMIT {limit}"#,
            chain = chain_label,
            block_month_min = block_month_min,
            from_block = from_block,
            to_block = to_block,
            limit = args.top * 3,
        );

        eprintln!(
            "Querying Dune for liquidation blocks on {} (blocks {}–{})...",
            chain, from_block, to_block
        );

        match client.execute_raw_sql(&sql).await {
            Ok(result) => {
                if let Some(ref r) = result.result {
                    for row in &r.rows {
                        if let Some((block, count)) = parse_block_count(row, "block_number", "liq_count") {
                            if block > 0 {
                                *block_scores.entry(block).or_insert(0) += count;
                            }
                        }
                    }
                    eprintln!("  Found {} blocks with liquidations", r.rows.len());
                }
            }
            Err(e) => {
                eprintln!("  Liquidation query failed: {}", e);
            }
        }
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    if find_flash_loans {
        let sql = format!(
            r#"SELECT block_number, COUNT(*) AS flash_count
FROM lending.flashloans
WHERE blockchain = '{chain}'
  AND block_month >= DATE '{block_month_min}'
  AND block_number >= {from_block}
  AND block_number <= {to_block}
GROUP BY block_number
ORDER BY flash_count DESC
LIMIT {limit}"#,
            chain = chain_label,
            block_month_min = block_month_min,
            from_block = from_block,
            to_block = to_block,
            limit = args.top * 3,
        );

        eprintln!(
            "Querying Dune for flash loan blocks on {} (blocks {}–{})...",
            chain, from_block, to_block
        );

        match client.execute_raw_sql(&sql).await {
            Ok(result) => {
                if let Some(ref r) = result.result {
                    for row in &r.rows {
                        if let Some((block, count)) = parse_block_count(row, "block_number", "flash_count") {
                            if block > 0 {
                                *block_scores.entry(block).or_insert(0) += count;
                            }
                        }
                    }
                    eprintln!("  Found {} blocks with flash loans", r.rows.len());
                }
            }
            Err(e) => {
                eprintln!("  Flash loan query failed: {}", e);
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
