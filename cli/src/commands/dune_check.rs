use crate::cli::DuneCheckArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::DuneClient;

pub async fn cmd_dune_check(config: &Config, args: &DuneCheckArgs) -> anyhow::Result<()> {
    let api_key = args
        .dune_api_key
        .clone()
        .or_else(|| config.dune_api_key.clone())
        .ok_or_else(|| anyhow::anyhow!(
            "No Dune API key found. Set it in mev-scout.toml (dune_api_key = \"...\") or pass --dune-api-key"
        ))?;

    let client = DuneClient::new(api_key);
    let block = args.block;
    let chain = &args.chain;

    let sql = format!(
        r#"SELECT
  project,
  COUNT(DISTINCT tx_hash) AS tx_count,
  COUNT(*) AS swap_count
FROM dex.trades
WHERE blockchain = '{chain}'
  AND block_number = {block}
GROUP BY project
ORDER BY tx_count DESC"#,
        chain = chain,
        block = block
    );

    println!(
        "Querying Dune for DEX trades on {} block #{}...\n",
        chain, block
    );

    let result = client.execute_raw_sql(&sql).await?;

    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => {
            println!("No results returned from Dune.");
            return Ok(());
        }
    };

    if rows.is_empty() {
        println!("No DEX trades found in block #{} on {}.", block, chain);
        return Ok(());
    }

    let mut total_txns = 0u64;
    let mut total_swaps = 0u64;
    let mut uni_v2_txns = 0u64;
    let mut uni_v3_txns = 0u64;

    println!("{:<22} {:>15} {:>15}", "Project", "Tx Count", "Swap Count");
    println!("{}", "-".repeat(54));

    for row in rows {
        let project = row
            .get("project")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let tx_count = row
            .get("tx_count")
            .and_then(|v| {
                if let Some(n) = v.as_u64() {
                    return Some(n);
                }
                v.as_str().and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(0);
        let swap_count = row
            .get("swap_count")
            .and_then(|v| {
                if let Some(n) = v.as_u64() {
                    return Some(n);
                }
                v.as_str().and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(0);

        total_txns += tx_count;
        total_swaps += swap_count;

        let proj_lower = project.to_lowercase();
        if proj_lower.contains("uniswap") || proj_lower.contains("quickswap") {
            if proj_lower.contains("v3") || proj_lower.contains("v2") {
                if proj_lower.contains("v2") { uni_v2_txns += tx_count; }
                if proj_lower.contains("v3") { uni_v3_txns += tx_count; }
            } else {
                uni_v2_txns += tx_count;
            }
        }

        println!(
            "{:<22} {:>15} {:>15}",
            project, tx_count, swap_count
        );
    }

    println!("{}", "-".repeat(54));
    println!(
        "{:<22} {:>15} {:>15}",
        "TOTAL (all DEX)", total_txns, total_swaps
    );
    println!();
    println!("Uniswap V2/V3 forks (QuickSwap etc.):");
    println!("  Unique transactions: {}", uni_v2_txns + uni_v3_txns);
    if uni_v2_txns > 0 {
        println!("  V2-type:              {} txns", uni_v2_txns);
    }
    if uni_v3_txns > 0 {
        println!("  V3-type:              {} txns", uni_v3_txns);
    }

    Ok(())
}
