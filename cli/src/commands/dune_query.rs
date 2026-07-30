use anyhow::Context;
use crate::cli::DuneQueryArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::client::DuneClient;
use mev_scout_core::dune::queries;
use mev_scout_core::dune::util::{approx_block_month_min, dune_chain_label};

/// Query metadata and SQL template.
struct QueryInfo {
    name: &'static str,
    description: &'static str,
    required: &'static [&'static str],
    sql: &'static str,
}

fn all_queries() -> Vec<QueryInfo> {
    let mut q = Vec::new();
    macro_rules! q {
        ($name:ident, $desc:expr, $req:expr) => {
            q.push(QueryInfo {
                name: stringify!($name),
                description: $desc,
                required: $req,
                sql: queries::$name,
            });
        };
    }
    // Section 1: Pool Discovery
    q!(QUERY_V2_POOLS_BY_FACTORY, "V2-style pools via dex.trades", &["chain", "from_block", "to_block"]);
    q!(QUERY_V3_POOLS_BY_FACTORY, "V3 pools via dex.trades", &["chain", "from_block", "to_block"]);
    q!(QUERY_CURVE_POOLS, "Curve pools via PoolAdded events", &["chain", "from_block", "to_block"]);
    q!(QUERY_BALANCER_POOLS, "Balancer V2 pools via PoolRegistered event", &["chain", "from_block", "to_block"]);
    q!(QUERY_ALL_ACTIVE_POOLS, "All active DEX pools from dex.trades", &["chain", "from_block", "to_block"]);
    q!(QUERY_POOLS_WITH_METADATA, "Pools with token symbols and decimals", &["chain", "from_block", "to_block"]);
    q!(QUERY_POOLS_BY_FACTORY_ADDRESS, "Pools by specific factory address", &["chain", "from_block", "to_block", "factory_address"]);
    // Section 2: Trade & Swap Analysis
    q!(QUERY_TRADES_IN_BLOCK, "All DEX trades in a specific block", &["chain", "block"]);
    q!(QUERY_TRADES_IN_RANGE, "All DEX trades in a block range", &["chain", "from_block", "to_block"]);
    q!(QUERY_TRADES_BY_POOL, "Trades involving a specific pool", &["chain", "from_block", "to_block", "pool_address"]);
    q!(QUERY_TRADES_BY_TOKEN_PAIR, "Trades for a specific token pair", &["chain", "from_block", "to_block", "token_in", "token_out"]);
    q!(QUERY_LARGE_SWAPS, "Large swaps (whale detection)", &["chain", "from_block", "to_block", "min_usd"]);
    q!(QUERY_VERIFY_TRADE_BY_TX, "Verify a specific trade by tx_hash", &["chain", "block", "tx_hash"]);
    // Section 3: MEV Detection
    q!(QUERY_SANDWICHES_BY_RANGE, "Sandwich attacks in a block range", &["chain", "from_block", "to_block"]);
    q!(QUERY_SANDWICHES_BY_BLOCK, "Sandwich attacks in a specific block", &["chain", "block"]);
    q!(QUERY_SANDWICHES_BY_TIME, "Sandwich attacks in a time range", &["chain", "from_time", "to_time"]);
    q!(QUERY_SANDWICHED_VICTIMS_BY_RANGE, "Victim trades that were sandwiched", &["chain", "from_block", "to_block"]);
    q!(QUERY_ARBITRAGES_BY_RANGE, "Arbitrage transactions in a block range", &["chain", "from_block", "to_block"]);
    q!(QUERY_ARBITRAGES_BY_BLOCK, "Arbitrage transactions in a specific block", &["chain", "block"]);
    q!(QUERY_ARBITRAGES_BY_TIME, "Arbitrage transactions in a time range", &["chain", "from_time", "to_time"]);
    q!(QUERY_FLASH_LOANS_BY_RANGE, "Flash loan events in a block range", &["chain", "from_block", "to_block"]);
    q!(QUERY_FLASH_LOANS_BY_BLOCK, "Flash loans in a specific block", &["chain", "block"]);
    q!(QUERY_AAVE_V3_LIQUIDATIONS, "Aave V3 liquidation events", &["chain", "from_block", "to_block"]);
    q!(QUERY_AAVE_V3_LIQUIDATIONS_BY_BLOCK, "Aave V3 liquidations in a specific block", &["chain", "block"]);
    q!(QUERY_COMPOUND_V3_LIQUIDATIONS, "Compound V3 liquidation events", &["chain", "from_block", "to_block"]);
    q!(QUERY_LIQUIDATIONS_ALL, "Combined liquidation events (all protocols)", &["chain", "from_block", "to_block"]);
    q!(QUERY_LIQUIDATIONS_BY_BLOCK, "Combined liquidations in a specific block", &["chain", "block"]);
    q!(QUERY_VERIFY_SANDWICH, "Verify if a tx is part of a sandwich", &["chain", "block", "tx_hash"]);
    q!(QUERY_FAILED_TXS, "Failed (reverted) transactions", &["chain", "from_block", "to_block"]);
    q!(QUERY_FAILED_TXS_BY_BLOCK, "Failed transactions in a specific block", &["chain", "block"]);
    // Section 4: Token & Price Data
    q!(QUERY_TOKEN_METADATA, "ERC20 token metadata", &["chain", "token_list"]);
    q!(QUERY_ALL_TOKENS, "All known tokens on a chain", &["chain"]);
    q!(QUERY_TOKEN_PRICE_AT_BLOCK, "Historical USD price at block time", &["chain", "token_address", "block_timestamp"]);
    q!(QUERY_TOKEN_PRICE_HISTORY, "Price history over a time window", &["chain", "token_address", "from_time", "to_time"]);
    q!(QUERY_TOKEN_PRICE_LATEST, "Latest USD price for a token", &["chain", "token_address"]);
    // Section 5: Block & Gas Data
    q!(QUERY_BLOCK_METADATA, "Block metadata (timestamp, gas, tx count)", &["chain", "from_block", "to_block"]);
    q!(QUERY_SINGLE_BLOCK, "Metadata for a single block", &["chain", "block"]);
    q!(QUERY_GAS_PRICE_HISTORY, "Gas price distribution stats per block", &["chain", "from_block", "to_block"]);
    // Section 6: Pattern Analysis
    q!(QUERY_SANDWICH_PATTERN, "Detect sandwich pattern in a block", &["chain", "block"]);
    q!(QUERY_JIT_PATTERN, "Detect JIT liquidity pattern", &["chain", "block"]);
    q!(QUERY_HIGH_VALUE_BLOCKS, "Blocks with high MEV value", &["chain", "from_block", "to_block"]);
    q!(QUERY_POOL_LIQUIDITY, "Pool liquidity snapshots", &["chain", "to_block"]);
    q!(QUERY_GAS_BY_HOUR, "Hourly average gas price", &["chain", "from_time", "to_time"]);
    q!(QUERY_WHALE_TRANSFERS, "Large token transfers (whale detection)", &["chain", "from_block", "to_block", "min_usd"]);
    q!(QUERY_WHALE_TRANSFERS_BY_BLOCK, "Large transfers in a specific block", &["chain", "block", "min_usd"]);
    // Section 7: Cross-Chain & Aggregation
    q!(QUERY_BRIDGE_FLOWS, "Cross-chain bridge transfer volumes", &["chain", "from_time", "to_time"]);
    q!(QUERY_BRIDGE_FLOWS_NET, "Cross-chain bridge net flows", &["chain", "from_time", "to_time"]);
    q!(QUERY_TOKEN_PRICE_VIA_TRADES, "Token price via nearby trades", &["chain", "token_address", "block_number", "from_block", "to_block"]);
    q!(QUERY_AGGREGATOR_TRADES_IN_RANGE, "Aggregator-routed trades (1inch, 0x, etc.)", &["chain", "from_block", "to_block"]);
    q!(QUERY_LABELS_BY_ADDRESSES, "Address labels from Dune", &["chain", "address_list"]);
    q!(QUERY_LABELS_BY_CATEGORY, "Address labels by category", &["chain", "category"]);
    q!(QUERY_LENDING_BORROW_BY_RANGE, "Lending borrow events", &["chain", "from_block", "to_block"]);
    q!(QUERY_LENDING_SUPPLY_BY_RANGE, "Lending supply events", &["chain", "from_block", "to_block"]);
    q!(QUERY_DEX_FLASH_LOANS_BY_RANGE, "DEX-native flash loans", &["chain", "from_block", "to_block"]);
    q!(QUERY_UTILS_DAYS, "Continuous days from utils.days", &["chain", "from_time", "to_time"]);
    q!(QUERY_UTILS_HOURS, "Continuous hours from utils.hours", &["chain", "from_time", "to_time"]);
    // Section 8: Strategy Validation
    q!(VALIDATE_SKIM_CAPTURE, "Validate skim() capture opportunities (V2 balance drift)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_SYNC_RACE, "Validate sync() race opportunities (defensive sync calls)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_INIT_PRICE_SNIPE, "Validate init price snipe opportunities (V3 mispriced pools)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_BACKRUN, "Validate backrunning opportunities (multi-pool txs after large swaps)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_LONG_TAIL_ARB, "Validate long-tail token arbitrage (low-liquidity multi-pool txs)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_STABLECOIN_DEPEG, "Validate stablecoin depeg arbitrage (Curve price deviations)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_CURVE_IMBALANCE, "Validate Curve pool imbalance: pools with balances deviating from peg", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_CURVE_IMBALANCE_V2, "Validate Curve imbalance using curvefi_polygon per-pool tables", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_LST_DEPEG_LIQ, "Validate LST depeg collateral liquidation (AAVE LST-collateral liqs)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_MAKERDAO_CLIP, "Validate MakerDAO Clip Dutch auction take() events", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_MAKERDAO_KICK, "Validate MakerDAO OSM kick() events (via lending.borrow)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_GMX_V1_KEEPER, "Validate GMX v1 keeper race (via lending.borrow)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_GMX_V2_ADL, "Validate GMX V2 ADL front-run events (via lending.borrow)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_LIQUITY_RECOVERY, "Validate Liquity recovery mode cascade (trove liquidations)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_SYNTHETIX_LIQ, "Validate Synthetix liquidation events (via lending.borrow)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_PERP_KEEPER, "Validate Gains Network keeper (via lending.borrow)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_FLASH_LIQ_PROFIT, "Validate flash loan atomic liquidation (flash + liq in same tx)", &["chain", "from_block", "to_block"]);
    q!(VALIDATE_JIT_FEE_CAPTURE, "Validate JIT liquidity fee capture (V3 Mint/Swap/Burn)", &["chain", "from_block", "to_block"]);
    q!(DISCOVER_CLIP_COLUMNS, "Show columns of maker Clipper_evt_Take", &[]);
    q!(DISCOVER_TRY_CLIP, "SELECT * FROM maker Clipper_evt_Take LIMIT 1", &[]);
    q!(DISCOVER_TRY_GMX, "Find GMX schemas on Arbitrum", &[]);
    q!(DISCOVER_TRY_GAINS, "Find Gains schemas on Arbitrum", &[]);
    q!(DISCOVER_TRY_SYNX, "Find Synthetix schemas on Ethereum", &[]);
    q!(DISCOVER_MAKER_TABLES, "Find MakerDAO tables", &[]);
    q!(DISCOVER_ALL_MAKER, "SHOW TABLES FROM maker_ethereum", &[]);
    q!(DISCOVER_ALL_GMX_ARB, "SHOW TABLES FROM gmx_arbitrum", &[]);
    q!(DISCOVER_ALL_GMX_ROUTER_ARB, "SHOW TABLES FROM gmx_router_arbitrum", &[]);
    q!(DISCOVER_ALL_GAINS_ARB, "SHOW TABLES FROM gains_network_arbitrum", &[]);
    q!(DISCOVER_ALL_SYNX_ETH, "SHOW TABLES FROM synthetix_ethereum", &[]);
    q!(DISCOVER_ALL_CURVE_POLY, "SHOW TABLES FROM curve_polygon", &[]);
    q!(DISCOVER_ALL_UNIV2_POLY, "SHOW SCHEMAS LIKE uniswap v2 polygon", &[]);
    q!(DISCOVER_MAKER_FLIP, "SHOW TABLES FROM maker_ethereum LIKE Vow", &[]);
    q!(DISCOVER_MAKER_CLIPPER, "SHOW TABLES FROM maker_ethereum LIKE Clip", &[]);
    q!(DISCOVER_GMX_ARB_SHOW, "SHOW SCHEMAS LIKE gmx arbitrum", &[]);
    q!(DISCOVER_GMX_V2_ARB_SHOW, "SHOW SCHEMAS LIKE gmx_router arbitrum", &[]);
    q!(DISCOVER_GAINS_ARB_SHOW, "SHOW SCHEMAS LIKE gains arbitrum", &[]);
    q!(DISCOVER_SYNX_ETH_SHOW, "SHOW SCHEMAS LIKE synthetix ethereum", &[]);
    q!(DISCOVER_CURVE_POLY_SHOW, "SHOW SCHEMAS LIKE curve polygon", &[]);
    q!(DISCOVER_GMX_LIQ_TABLES, "SHOW TABLES FROM gmx_arbitrum LIKE Liquidat", &[]);
    q!(DISCOVER_GAINS_LIQ_TABLES, "SHOW TABLES FROM gains_network_arbitrum LIKE Liquidat", &[]);
    q!(DISCOVER_SYNX_LIQ_TABLES, "SHOW TABLES FROM synthetix_ethereum LIKE Liquidat", &[]);
    q!(DISCOVER_SYNX_V3_LIQ_TABLES, "SHOW TABLES FROM synthetix_v3_ethereum LIKE Liquidat", &[]);
    q!(DISCOVER_MAKER_VOW_FLIP, "SHOW TABLES FROM maker_ethereum LIKE Flip", &[]);
    q!(DISCOVER_MAKER_CLIPPER_TAKE, "SHOW TABLES FROM maker_ethereum LIKE Take", &[]);
    q!(DISCOVER_GMX_V2_TABLES, "SHOW TABLES FROM gmx_v2_arbitrum LIKE Order", &[]);
    q!(DISCOVER_GMX_V21_TABLES, "SHOW TABLES FROM gmx_v21_arbitrum LIKE Order", &[]);
    q!(DISCOVER_MAKER_FULL, "Find MakerDAO evt tables with art column", &[]);
    q!(DISCOVER_MAKER_V2_FULL, "Find MakerDAO evt tables with usr column", &[]);
    q!(DISCOVER_GMX_VAULT_FULL, "Find GMX tables with feeAmount column", &[]);
    q!(DISCOVER_GMX_V2_ORDER_FULL, "Find GMX v2 evt tables", &[]);
    q!(DISCOVER_GAINS_LIQ_FULL, "Find Gains arb evt tables", &[]);
    q!(DISCOVER_SYNX_LIQ_FULL, "Find Synthetix eth liquidation tables", &[]);
    q!(DISCOVER_CURVE_TOKEN_EXCHANGE, "Find Curve polygon TokenExchange tables", &[]);
    q!(DISCOVER_UNIV2_SYNC, "Find UniV2 polygon Sync tables", &[]);
    q!(DISCOVER_MAKER_EVT_TABLES, "Find MakerDAO evt tables via information_schema", &[]);
    q!(DISCOVER_GMX_VAULT_EVT, "Find GMX arbitrum evt tables", &[]);
    q!(DISCOVER_GMX_V2_EVT, "Find GMX v2 arbitrum evt tables", &[]);
    q!(DISCOVER_GMX_V21_EVT, "Find GMX v21 arbitrum evt tables", &[]);
    q!(DISCOVER_GAINS_EVT, "Find Gains network arbitrum evt tables", &[]);
    q!(DISCOVER_SYNX_EVT, "Find Synthetix ethereum evt tables", &[]);
    q!(DISCOVER_SYNX_V3_EVT, "Find Synthetix v3 ethereum evt tables", &[]);
    q!(DISCOVER_CURVEFI_POLY_TABLES, "SHOW TABLES FROM curvefi_polygon", &[]);
    q!(DISCOVER_CURVEFI_POLY_LIKE, "SHOW TABLES FROM curvefi_polygon LIKE TokenExchange", &[]);
    q!(DISCOVER_MAKERDAO_SCHEMA_SHOW, "SHOW SCHEMAS LIKE maker ethereum", &[]);
    q!(DISCOVER_GMX_LIQ_IN_V2, "SHOW TABLES FROM gmx_v2_arbitrum LIKE Liquidat", &[]);
    q!(DISCOVER_GMX_ORDER_IN_V2, "SHOW TABLES FROM gmx_v2_arbitrum LIKE Order", &[]);
    q!(DISCOVER_GMX_ADLP_IN_V2, "SHOW TABLES FROM gmx_v2_arbitrum LIKE Adl", &[]);
    q!(DISCOVER_GMX_ADLP_IN_V2_UPPER, "SHOW TABLES FROM gmx_v2_arbitrum LIKE ADL", &[]);
    q!(DISCOVER_GAINS_ARB_TABLES, "SHOW TABLES FROM gains_network_arbitrum LIKE Liquidat", &[]);
    q!(DISCOVER_GAINS_GTOKEN, "SHOW TABLES FROM gains_network_arbitrum LIKE GToken", &[]);
    q!(DISCOVER_GAINS_DUIF, "SHOW TABLES FROM gains_network_arbitrum LIKE duif", &[]);
    q!(DISCOVER_GAINS_DN, "SHOW TABLES FROM gains_network_arbitrum LIKE dncdnc", &[]);
    q!(DISCOVER_SYNX_V3_COLS, "SELECT * FROM synthetix_v3.core_evt_liquidation LIMIT 5", &[]);
    q!(DISCOVER_GMX_V1_VAULT_LIQ, "SELECT * FROM gmx_arbitrum.Vault_evt_Liquidation LIMIT 5", &[]);
    q!(DISCOVER_GMX_V2_LIQ_HANDLER, "SELECT * FROM gmx_v2_arbitrum.liquidationhandler_evt_oracleerror LIMIT 5", &[]);
    q!(DISCOVER_GMX_V2_ADL_HANDLER, "SELECT * FROM gmx_v2_arbitrum.adlhandler_evt_oracleerror LIMIT 5", &[]);
    q
}

fn get_query_sql(name: &str) -> Option<&'static str> {
    Some(all_queries().into_iter().find(|q| q.name == name)?.sql)
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
        sql = sql.replace("{block_month_min}", &block_month_min);
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
        .or_else(|| config.dune.dune_api_key.clone())
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

    // Validate required params (skip for discovery/ad-hoc queries not in the registry)
    let all = all_queries();
    if let Some(info) = all.iter().find(|q| q.name == query_name) {
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
                _ => false,
            }
        }).copied().collect::<Vec<_>>();
        if !missing.is_empty() {
            anyhow::bail!(
                "Missing required params for {}: {}",
                query_name,
                missing.join(", ")
            );
        }
    }

    let sql = render_sql(sql_template, &args.chain, args);

    eprintln!("Running {} on {}...", query_name, args.chain);
    eprintln!("SQL:\n{}\n", sql);

    let result = client.execute_raw_sql(&sql).await
        .context("Dune query execution failed")?;

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
