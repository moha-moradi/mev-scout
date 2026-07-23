use crate::cli::DuneQueryArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::client::DuneClient;
use mev_scout_core::dune::queries;

/// Query metadata: name, description, required params, optional params.
struct QueryInfo {
    name: &'static str,
    description: &'static str,
    required: &'static [&'static str],
    optional: &'static [&'static str],
}

fn all_queries() -> Vec<QueryInfo> {
    vec![
        // Section 1: Pool Discovery
        QueryInfo {
            name: "QUERY_V2_POOLS_BY_FACTORY",
            description: "V2-style pools via dex.trades",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_V3_POOLS_BY_FACTORY",
            description: "V3 pools via dex.trades",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_CURVE_POOLS",
            description: "Curve pools via PoolAdded events",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_BALANCER_POOLS",
            description: "Balancer V2 pools via PoolRegistered event",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_ALL_ACTIVE_POOLS",
            description: "All active DEX pools from dex.trades",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_POOLS_WITH_METADATA",
            description: "Pools with token symbols and decimals",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_POOLS_BY_FACTORY_ADDRESS",
            description: "Pools by specific factory address",
            required: &["chain", "from_block", "to_block", "factory_address"],
            optional: &[],
        },
        // Section 2: Trade & Swap Analysis
        QueryInfo {
            name: "QUERY_TRADES_IN_BLOCK",
            description: "All DEX trades in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_TRADES_IN_RANGE",
            description: "All DEX trades in a block range",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_TRADES_BY_POOL",
            description: "Trades involving a specific pool",
            required: &["chain", "from_block", "to_block", "pool_address"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_TRADES_BY_TOKEN_PAIR",
            description: "Trades for a specific token pair",
            required: &["chain", "from_block", "to_block", "token_in", "token_out"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_LARGE_SWAPS",
            description: "Large swaps (whale detection)",
            required: &["chain", "from_block", "to_block", "min_usd"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_VERIFY_TRADE_BY_TX",
            description: "Verify a specific trade by tx_hash",
            required: &["chain", "block", "tx_hash"],
            optional: &["block_month_min"],
        },
        // Section 3: MEV Detection
        QueryInfo {
            name: "QUERY_SANDWICHES_BY_RANGE",
            description: "Sandwich attacks in a block range",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_SANDWICHES_BY_BLOCK",
            description: "Sandwich attacks in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_SANDWICHES_BY_TIME",
            description: "Sandwich attacks in a time range",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_SANDWICHED_VICTIMS_BY_RANGE",
            description: "Victim trades that were sandwiched",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_ARBITRAGES_BY_RANGE",
            description: "Arbitrage transactions in a block range",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_ARBITRAGES_BY_BLOCK",
            description: "Arbitrage transactions in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_ARBITRAGES_BY_TIME",
            description: "Arbitrage transactions in a time range",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_FLASH_LOANS_BY_RANGE",
            description: "Flash loan events in a block range",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_FLASH_LOANS_BY_BLOCK",
            description: "Flash loans in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_AAVE_V3_LIQUIDATIONS",
            description: "Aave V3 liquidation events",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_AAVE_V3_LIQUIDATIONS_BY_BLOCK",
            description: "Aave V3 liquidations in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_COMPOUND_V3_LIQUIDATIONS",
            description: "Compound V3 liquidation events",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_LIQUIDATIONS_ALL",
            description: "Combined liquidation events (all protocols)",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_LIQUIDATIONS_BY_BLOCK",
            description: "Combined liquidations in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_VERIFY_SANDWICH",
            description: "Verify if a tx is part of a sandwich",
            required: &["chain", "block", "tx_hash"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_FAILED_TXS",
            description: "Failed (reverted) transactions",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_FAILED_TXS_BY_BLOCK",
            description: "Failed transactions in a specific block",
            required: &["chain", "block"],
            optional: &[],
        },
        // Section 4: Token & Price Data
        QueryInfo {
            name: "QUERY_TOKEN_METADATA",
            description: "ERC20 token metadata",
            required: &["chain", "token_list"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_ALL_TOKENS",
            description: "All known tokens on a chain",
            required: &["chain"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_TOKEN_PRICE_AT_BLOCK",
            description: "Historical USD price at block time",
            required: &["chain", "token_address", "block_timestamp"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_TOKEN_PRICE_HISTORY",
            description: "Price history over a time window",
            required: &["chain", "token_address", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_TOKEN_PRICE_LATEST",
            description: "Latest USD price for a token",
            required: &["chain", "token_address"],
            optional: &[],
        },
        // Section 5: Block & Gas Data
        QueryInfo {
            name: "QUERY_BLOCK_METADATA",
            description: "Block metadata (timestamp, gas, tx count)",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_SINGLE_BLOCK",
            description: "Metadata for a single block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_GAS_PRICE_HISTORY",
            description: "Gas price distribution stats per block",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        // Section 6: Pattern Analysis
        QueryInfo {
            name: "QUERY_SANDWICH_PATTERN",
            description: "Detect sandwich pattern in a block",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_JIT_PATTERN",
            description: "Detect JIT liquidity pattern",
            required: &["chain", "block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_HIGH_VALUE_BLOCKS",
            description: "Blocks with high MEV value",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_POOL_LIQUIDITY",
            description: "Pool liquidity snapshots",
            required: &["chain", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_GAS_BY_HOUR",
            description: "Hourly average gas price",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_WHALE_TRANSFERS",
            description: "Large token transfers (whale detection)",
            required: &["chain", "from_block", "to_block", "min_usd"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_WHALE_TRANSFERS_BY_BLOCK",
            description: "Large transfers in a specific block",
            required: &["chain", "block", "min_usd"],
            optional: &[],
        },
        // Section 7: Cross-Chain & Aggregation
        QueryInfo {
            name: "QUERY_BRIDGE_FLOWS",
            description: "Cross-chain bridge transfer volumes",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_BRIDGE_FLOWS_NET",
            description: "Cross-chain bridge net flows",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_TOKEN_PRICE_VIA_TRADES",
            description: "Token price via nearby trades",
            required: &["chain", "token_address", "block_number", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_AGGREGATOR_TRADES_IN_RANGE",
            description: "Aggregator-routed trades (1inch, 0x, etc.)",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_LABELS_BY_ADDRESSES",
            description: "Address labels from Dune",
            required: &["chain", "address_list"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_LABELS_BY_CATEGORY",
            description: "Address labels by category",
            required: &["chain", "category"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_LENDING_BORROW_BY_RANGE",
            description: "Lending borrow events",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_LENDING_SUPPLY_BY_RANGE",
            description: "Lending supply events",
            required: &["chain", "from_block", "to_block"],
            optional: &["block_month_min"],
        },
        QueryInfo {
            name: "QUERY_DEX_FLASH_LOANS_BY_RANGE",
            description: "DEX-native flash loans",
            required: &["chain", "from_block", "to_block"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_UTILS_DAYS",
            description: "Continuous days from utils.days",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
        QueryInfo {
            name: "QUERY_UTILS_HOURS",
            description: "Continuous hours from utils.hours",
            required: &["chain", "from_time", "to_time"],
            optional: &[],
        },
    ]
}

fn get_query_sql(name: &str) -> Option<&'static str> {
    match name {
        "QUERY_V2_POOLS_BY_FACTORY" => Some(queries::QUERY_V2_POOLS_BY_FACTORY),
        "QUERY_V3_POOLS_BY_FACTORY" => Some(queries::QUERY_V3_POOLS_BY_FACTORY),
        "QUERY_CURVE_POOLS" => Some(queries::QUERY_CURVE_POOLS),
        "QUERY_BALANCER_POOLS" => Some(queries::QUERY_BALANCER_POOLS),
        "QUERY_ALL_ACTIVE_POOLS" => Some(queries::QUERY_ALL_ACTIVE_POOLS),
        "QUERY_POOLS_WITH_METADATA" => Some(queries::QUERY_POOLS_WITH_METADATA),
        "QUERY_POOLS_BY_FACTORY_ADDRESS" => Some(queries::QUERY_POOLS_BY_FACTORY_ADDRESS),
        "QUERY_TRADES_IN_BLOCK" => Some(queries::QUERY_TRADES_IN_BLOCK),
        "QUERY_TRADES_IN_RANGE" => Some(queries::QUERY_TRADES_IN_RANGE),
        "QUERY_TRADES_BY_POOL" => Some(queries::QUERY_TRADES_BY_POOL),
        "QUERY_TRADES_BY_TOKEN_PAIR" => Some(queries::QUERY_TRADES_BY_TOKEN_PAIR),
        "QUERY_LARGE_SWAPS" => Some(queries::QUERY_LARGE_SWAPS),
        "QUERY_VERIFY_TRADE_BY_TX" => Some(queries::QUERY_VERIFY_TRADE_BY_TX),
        "QUERY_SANDWICHES_BY_RANGE" => Some(queries::QUERY_SANDWICHES_BY_RANGE),
        "QUERY_SANDWICHES_BY_BLOCK" => Some(queries::QUERY_SANDWICHES_BY_BLOCK),
        "QUERY_SANDWICHES_BY_TIME" => Some(queries::QUERY_SANDWICHES_BY_TIME),
        "QUERY_SANDWICHED_VICTIMS_BY_RANGE" => Some(queries::QUERY_SANDWICHED_VICTIMS_BY_RANGE),
        "QUERY_ARBITRAGES_BY_RANGE" => Some(queries::QUERY_ARBITRAGES_BY_RANGE),
        "QUERY_ARBITRAGES_BY_BLOCK" => Some(queries::QUERY_ARBITRAGES_BY_BLOCK),
        "QUERY_ARBITRAGES_BY_TIME" => Some(queries::QUERY_ARBITRAGES_BY_TIME),
        "QUERY_FLASH_LOANS_BY_RANGE" => Some(queries::QUERY_FLASH_LOANS_BY_RANGE),
        "QUERY_FLASH_LOANS_BY_BLOCK" => Some(queries::QUERY_FLASH_LOANS_BY_BLOCK),
        "QUERY_AAVE_V3_LIQUIDATIONS" => Some(queries::QUERY_AAVE_V3_LIQUIDATIONS),
        "QUERY_AAVE_V3_LIQUIDATIONS_BY_BLOCK" => Some(queries::QUERY_AAVE_V3_LIQUIDATIONS_BY_BLOCK),
        "QUERY_COMPOUND_V3_LIQUIDATIONS" => Some(queries::QUERY_COMPOUND_V3_LIQUIDATIONS),
        "QUERY_LIQUIDATIONS_ALL" => Some(queries::QUERY_LIQUIDATIONS_ALL),
        "QUERY_LIQUIDATIONS_BY_BLOCK" => Some(queries::QUERY_LIQUIDATIONS_BY_BLOCK),
        "QUERY_VERIFY_SANDWICH" => Some(queries::QUERY_VERIFY_SANDWICH),
        "QUERY_FAILED_TXS" => Some(queries::QUERY_FAILED_TXS),
        "QUERY_FAILED_TXS_BY_BLOCK" => Some(queries::QUERY_FAILED_TXS_BY_BLOCK),
        "QUERY_TOKEN_METADATA" => Some(queries::QUERY_TOKEN_METADATA),
        "QUERY_ALL_TOKENS" => Some(queries::QUERY_ALL_TOKENS),
        "QUERY_TOKEN_PRICE_AT_BLOCK" => Some(queries::QUERY_TOKEN_PRICE_AT_BLOCK),
        "QUERY_TOKEN_PRICE_HISTORY" => Some(queries::QUERY_TOKEN_PRICE_HISTORY),
        "QUERY_TOKEN_PRICE_LATEST" => Some(queries::QUERY_TOKEN_PRICE_LATEST),
        "QUERY_BLOCK_METADATA" => Some(queries::QUERY_BLOCK_METADATA),
        "QUERY_SINGLE_BLOCK" => Some(queries::QUERY_SINGLE_BLOCK),
        "QUERY_GAS_PRICE_HISTORY" => Some(queries::QUERY_GAS_PRICE_HISTORY),
        "QUERY_SANDWICH_PATTERN" => Some(queries::QUERY_SANDWICH_PATTERN),
        "QUERY_JIT_PATTERN" => Some(queries::QUERY_JIT_PATTERN),
        "QUERY_HIGH_VALUE_BLOCKS" => Some(queries::QUERY_HIGH_VALUE_BLOCKS),
        "QUERY_POOL_LIQUIDITY" => Some(queries::QUERY_POOL_LIQUIDITY),
        "QUERY_GAS_BY_HOUR" => Some(queries::QUERY_GAS_BY_HOUR),
        "QUERY_WHALE_TRANSFERS" => Some(queries::QUERY_WHALE_TRANSFERS),
        "QUERY_WHALE_TRANSFERS_BY_BLOCK" => Some(queries::QUERY_WHALE_TRANSFERS_BY_BLOCK),
        "QUERY_BRIDGE_FLOWS" => Some(queries::QUERY_BRIDGE_FLOWS),
        "QUERY_BRIDGE_FLOWS_NET" => Some(queries::QUERY_BRIDGE_FLOWS_NET),
        "QUERY_TOKEN_PRICE_VIA_TRADES" => Some(queries::QUERY_TOKEN_PRICE_VIA_TRADES),
        "QUERY_AGGREGATOR_TRADES_IN_RANGE" => Some(queries::QUERY_AGGREGATOR_TRADES_IN_RANGE),
        "QUERY_LABELS_BY_ADDRESSES" => Some(queries::QUERY_LABELS_BY_ADDRESSES),
        "QUERY_LABELS_BY_CATEGORY" => Some(queries::QUERY_LABELS_BY_CATEGORY),
        "QUERY_LENDING_BORROW_BY_RANGE" => Some(queries::QUERY_LENDING_BORROW_BY_RANGE),
        "QUERY_LENDING_SUPPLY_BY_RANGE" => Some(queries::QUERY_LENDING_SUPPLY_BY_RANGE),
        "QUERY_DEX_FLASH_LOANS_BY_RANGE" => Some(queries::QUERY_DEX_FLASH_LOANS_BY_RANGE),
        "QUERY_UTILS_DAYS" => Some(queries::QUERY_UTILS_DAYS),
        "QUERY_UTILS_HOURS" => Some(queries::QUERY_UTILS_HOURS),
        _ => None,
    }
}

fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}

fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let (genesis_ts, secs_per_block) = match chain {
        "ethereum" => (1438269988_i64, 12.0),
        "polygon" => (1591031691, 2.1),
        "bsc" => (1597734000, 3.0),
        "avalanche_c" => (1624402800, 2.0),
        "arbitrum" => (1630812600, 0.26),
        "base" => (1686787200, 2.0),
        "optimism" => (1631808000, 2.0),
        _ => (1609459200, 12.0),
    };
    let elapsed = block_number as f64 * secs_per_block;
    let approx_ts = genesis_ts + elapsed as i64;

    // Convert epoch to YYYY-MM-DD without chrono
    let days = approx_ts / 86400;
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

fn render_sql(
    template: &str,
    chain: &str,
    args: &DuneQueryArgs,
) -> String {
    let chain_label = dune_chain_label(chain);
    let mut sql = template.replace("{chain}", &chain_label);

    if let Some(from) = args.from_block {
        let block_month_min = approx_block_month_min(from, &chain_label);
        sql = sql.replace("{block_month_min}", &format!("'{}'", block_month_min));
        sql = sql.replace("{from_block}", &from.to_string());
    }
    if let Some(to) = args.to_block {
        sql = sql.replace("{to_block}", &to.to_string());
    }
    if let Some(block) = args.block {
        sql = sql.replace("{block_number}", &block.to_string());
    }
    if let Some(ref addr) = args.pool_address {
        sql = sql.replace("{pool_address}", addr);
    }
    if let Some(ref addr) = args.token_address {
        sql = sql.replace("{token_address}", addr);
    }
    if let Some(ref hash) = args.tx_hash {
        sql = sql.replace("{tx_hash}", hash);
    }
    if let Some(min) = args.min_usd {
        sql = sql.replace("{min_usd}", &min.to_string());
    }
    if let Some(ref addr) = args.factory_address {
        sql = sql.replace("{factory_address}", addr);
    }
    if let Some(ref time) = args.from_time {
        sql = sql.replace("{from_time}", time);
    }
    if let Some(ref time) = args.to_time {
        sql = sql.replace("{to_time}", time);
    }

    sql
}

fn print_table(rows: &[mev_scout_core::dune::types::DuneRow]) {
    if rows.is_empty() {
        println!("(no results)");
        return;
    }

    // Collect all column names
    let mut cols: Vec<String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if !cols.contains(&key.to_string()) {
                cols.push(key.to_string());
            }
        }
    }

    // Calculate column widths
    let mut widths: Vec<usize> = cols.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, col) in cols.iter().enumerate() {
            let val = row.get(col.as_str()).map(|v| {
                if v.is_string() {
                    v.as_str().unwrap_or("").to_string()
                } else {
                    v.to_string()
                }
            }).unwrap_or_default();
            if val.len() > widths[i] {
                widths[i] = val.len().min(50);
            }
        }
    }

    // Print header
    for (i, col) in cols.iter().enumerate() {
        print!("{:>width$}  ", col, width = widths[i]);
    }
    println!();

    // Print separator
    for w in &widths {
        print!("{:-<width$}  ", "", width = w);
    }
    println!();

    // Print rows (limit to 100 for display)
    let display_rows = rows.len().min(100);
    for row in &rows[..display_rows] {
        for (i, col) in cols.iter().enumerate() {
            let val = row.get(col.as_str()).map(|v| {
                if v.is_string() {
                    v.as_str().unwrap_or("").to_string()
                } else if v.is_null() {
                    "NULL".to_string()
                } else {
                    v.to_string()
                }
            }).unwrap_or_default();
            let truncated = if val.len() > 50 {
                format!("{}...", &val[..47])
            } else {
                val
            };
            print!("{:>width$}  ", truncated, width = widths[i]);
        }
        println!();
    }

    if rows.len() > 100 {
        println!("... ({} total rows, showing first 100)", rows.len());
    } else {
        println!("({} rows)", rows.len());
    }
}

pub async fn cmd_dune_query(config: &Config, args: &DuneQueryArgs) -> anyhow::Result<()> {
    // --list: print available queries
    if args.list {
        let queries = all_queries();
        println!("{:<45} {}", "Query Name", "Description");
        println!("{}", "-".repeat(90));
        for q in &queries {
            println!("{:<45} {}", q.name, q.description);
        }
        println!();
        println!("Total: {} queries", queries.len());
        return Ok(());
    }

    // Get API key
    let api_key = args
        .dune_api_key
        .clone()
        .or_else(|| config.dune_api_key.clone())
        .ok_or_else(|| anyhow::anyhow!(
            "No Dune API key. Set in mev-scout.toml (dune_api_key = \"...\") or pass --dune-api-key"
        ))?;

    let client = DuneClient::new(api_key);

    if args.all {
        // Run all queries that have required params satisfied
        let queries = all_queries();
        let mut executed = 0u32;
        let mut succeeded = 0u32;
        let mut failed = 0u32;

        for q in &queries {
            // Check if required params are available
            let can_run = q.required.iter().all(|param| {
                match *param {
                    "chain" => true,
                    "from_block" => args.from_block.is_some(),
                    "to_block" => args.to_block.is_some(),
                    "block" => args.block.is_some(),
                    "pool_address" => args.pool_address.is_some(),
                    "token_address" => args.token_address.is_some(),
                    "tx_hash" => args.tx_hash.is_some(),
                    "min_usd" => args.min_usd.is_some(),
                    "factory_address" => args.factory_address.is_some(),
                    "from_time" => args.from_time.is_some(),
                    "to_time" => args.to_time.is_some(),
                    "block_timestamp" => args.from_time.is_some() || args.block.is_some(),
                    "token_list" => false, // needs special input
                    "address_list" => false,
                    "category" => false,
                    _ => false,
                }
            });

            if !can_run {
                eprintln!("  SKIP {} (missing required params)", q.name);
                continue;
            }

            executed += 1;
            if let Some(sql_template) = get_query_sql(q.name) {
                let sql = render_sql(sql_template, &args.chain, args);
                eprintln!("Running {}...", q.name);

                match client.execute_raw_sql(&sql).await {
                    Ok(result) => {
                        if let Some(ref r) = result.result {
                            println!("\n=== {} ===", q.name);
                            println!("{}\n", q.description);
                            print_table(&r.rows);
                            succeeded += 1;
                        } else {
                            eprintln!("  {} returned no results", q.name);
                        }
                    }
                    Err(e) => {
                        eprintln!("  FAILED {}: {}", q.name, e);
                        failed += 1;
                    }
                }
            }
        }

        eprintln!("\nSummary: {} executed, {} succeeded, {} failed", executed, succeeded, failed);
        return Ok(());
    }

    // Run a specific query
    let query_name = args.query.as_deref().ok_or_else(|| anyhow::anyhow!(
        "Specify --query NAME, --list, or --all. Use --list to see available queries."
    ))?;

    let sql_template = get_query_sql(query_name).ok_or_else(|| {
        let valid: Vec<&str> = all_queries().iter().map(|q| q.name).collect();
        anyhow::anyhow!(
            "Unknown query '{}'. Use --list to see available queries.\nValid names: {}",
            query_name,
            valid.join(", ")
        )
    })?;

    // Validate required params
    let all = all_queries();
    let info = all.iter().find(|q| q.name == query_name).unwrap();
    let missing: Vec<&str> = info.required.iter().filter(|param| {
        match **param {
            "chain" => false,
            "from_block" => args.from_block.is_none(),
            "to_block" => args.to_block.is_none(),
            "block" => args.block.is_none(),
            "pool_address" => args.pool_address.is_none(),
            "token_address" => args.token_address.is_none(),
            "tx_hash" => args.tx_hash.is_none(),
            "min_usd" => args.min_usd.is_none(),
            "factory_address" => args.factory_address.is_none(),
            "from_time" => args.from_time.is_none(),
            "to_time" => args.to_time.is_none(),
            "block_timestamp" => args.from_time.is_none() && args.block.is_none(),
            "token_list" => true,
            "address_list" => true,
            "category" => true,
            _ => true,
        }
    }).copied().collect();

    if !missing.is_empty() {
        anyhow::bail!(
            "Missing required parameters for {}: {}. Use --help to see available flags.",
            query_name,
            missing.join(", ")
        );
    }

    let sql = render_sql(sql_template, &args.chain, args);

    eprintln!("Running {} on {}...", query_name, args.chain);
    eprintln!("SQL:\n{}\n", sql);

    let result = client.execute_raw_sql(&sql).await?;

    match result.result {
        Some(ref r) => {
            match args.output.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&r.rows)?);
                }
                "csv" => {
                    if r.rows.is_empty() {
                        println!("(no results)");
                    } else {
                        // CSV header
                        let mut cols: Vec<String> = Vec::new();
                        for key in r.rows[0].keys() {
                            cols.push(key.clone());
                        }
                        println!("{}", cols.join(","));
                        // CSV rows
                        for row in &r.rows {
                            let values: Vec<String> = cols.iter().map(|col| {
                                row.get(col.as_str()).map(|v| {
                                    if v.is_string() {
                                        format!("\"{}\"", v.as_str().unwrap_or(""))
                                    } else if v.is_null() {
                                        "".to_string()
                                    } else {
                                        v.to_string()
                                    }
                                }).unwrap_or_default()
                            }).collect();
                            println!("{}", values.join(","));
                        }
                    }
                }
                _ => {
                    print_table(&r.rows);
                }
            }
        }
        None => {
            println!("No results returned from Dune.");
        }
    }

    Ok(())
}
