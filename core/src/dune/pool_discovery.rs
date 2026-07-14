use std::collections::HashMap;

use alloy::primitives::Address;
use tracing;

use super::client::DuneClient;
use super::queries;
use crate::pool::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;

fn render_query(template: &str, chain: &str, from_block: u64, to_block: u64) -> String {
    let chain_label = dune_chain_label(chain);
    let block_month_min = approx_block_month_min(from_block, &chain_label);
    template
        .replace("{chain}", &chain_label)
        .replace("{block_month_min}", &block_month_min)
        .replace("{from_block}", &from_block.to_string())
        .replace("{to_block}", &to_block.to_string())
}

fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let (genesis_block, genesis_ts, secs_per_block) = match chain {
        "ethereum" => (0, 1438269988, 12.0),
        "polygon" => (0, 1591031691, 2.1),
        "bsc"      => (0, 1597734000, 3.0),
        "avalanche_c" => (0, 1624402800, 2.0),
        "arbitrum" => (0, 1630812600, 0.26),
        "base"     => (0, 1686787200, 2.0),
        "optimism" => (0, 1631808000, 2.0),
        _ => (0, 1609459200, 12.0),
    };
    let elapsed = (block_number.saturating_sub(genesis_block)) as f64 * secs_per_block;
    let approx_ts = genesis_ts as i64 + elapsed as i64;
    let naive = chrono::DateTime::from_timestamp(approx_ts, 0)
        .unwrap_or_default();
    naive.format("%Y-%m-%d").to_string()
}

/// Map of chain names to DuneSQL chain labels.
/// Returns a `String` to handle non-static mappings (e.g. "avalanche" → "avalanche_c").
pub fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}

/// Derive tick_spacing from a Uniswap V3 fee tier.
/// Standard mapping across most V3 forks (Uniswap, PancakeSwap, QuickSwap, etc.).
pub fn tick_spacing_from_fee(fee: u32) -> i32 {
    match fee {
        100 => 10,
        200 => 4,
        400 => 4,
        500 => 10,
        2500 => 50,
        3000 => 60,
        10000 => 200,
        _ => 60,
    }
}

/// Discover V2-style pools via the built-in Dune query (`QUERY_V2_POOLS_BY_FACTORY`).
///
/// Expected Dune columns: `pool_address`, `token0`, `token1`, `creation_block`, `factory` (optional).
pub async fn discover_v2_pools_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
    fee_override: u32,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let sql = render_query(queries::QUERY_V2_POOLS_BY_FACTORY, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;

    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut pools = Vec::with_capacity(rows.len());
    for row in rows {
        let address = DuneClient::col_as_address(row, "pool_address");
        let token0 = DuneClient::col_as_address(row, "token0");
        let token1 = DuneClient::col_as_address(row, "token1");
        let creation_block = DuneClient::col_as_u64(row, "creation_block").unwrap_or(0);

        if let (Some(addr), Some(t0), Some(t1)) = (address, token0, token1) {
            if t0 == Address::ZERO || t1 == Address::ZERO || t0 == t1 {
                continue;
            }
            pools.push(DiscoveredPool {
                address: addr,
                token0: t0,
                token1: t1,
                fee: fee_override,
                tick_spacing: None,
                dex_type: DexType::UniswapV2,
                creation_block,
                pool_id: None,
                factory: None,
            });
        }
    }

    tracing::info!(
        "Dune V2 discovery: found {} pools from QUERY_V2_POOLS_BY_FACTORY",
        pools.len(),
    );
    Ok(pools)
}

/// Discover V3 pools via the built-in Dune query (`QUERY_V3_POOLS_BY_FACTORY`).
///
/// Expected Dune columns: `pool_address`, `token0`, `token1`, `fee`, `tick_spacing`,
/// `creation_block`, `factory` (optional)
pub async fn discover_v3_pools_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let sql = render_query(queries::QUERY_V3_POOLS_BY_FACTORY, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;

    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut pools = Vec::with_capacity(rows.len());
    for row in rows {
        let address = DuneClient::col_as_address(row, "pool_address");
        let token0 = DuneClient::col_as_address(row, "token0");
        let token1 = DuneClient::col_as_address(row, "token1");
        let fee = DuneClient::col_as_u64(row, "fee").unwrap_or(3000) as u32;
        let tick_spacing = DuneClient::col_as_u64(row, "tick_spacing").map(|ts| ts as i32);
        let tick_spacing = tick_spacing.or_else(|| Some(tick_spacing_from_fee(fee)));
        let creation_block = DuneClient::col_as_u64(row, "creation_block").unwrap_or(0);

        if let (Some(addr), Some(t0), Some(t1)) = (address, token0, token1) {
            if t0 == Address::ZERO || t1 == Address::ZERO || t0 == t1 {
                continue;
            }
            pools.push(DiscoveredPool {
                address: addr,
                token0: t0,
                token1: t1,
                fee,
                tick_spacing,
                dex_type: DexType::UniswapV3,
                creation_block,
                pool_id: None,
                factory: None,
            });
        }
    }

    tracing::info!(
        "Dune V3 discovery: found {} pools from QUERY_V3_POOLS_BY_FACTORY",
        pools.len(),
    );
    Ok(pools)
}

/// Discover all active pools in a block range from `dex.trades` via Dune.
///
/// Uses the built-in `QUERY_ALL_ACTIVE_POOLS` query.
///
/// Expected Dune columns: `pool_address`, `token0`, `token1`,
/// `project`, `version`, `creation_block`, `last_active_block`, `fee`
pub async fn discover_active_pools_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let sql = render_query(queries::QUERY_ALL_ACTIVE_POOLS, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;

    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut seen = std::collections::HashSet::new();
    let mut pools = Vec::new();

    for row in rows {
        let address = DuneClient::col_as_address(row, "pool_address");
        let token0 = DuneClient::col_as_address(row, "token0");
        let token1 = DuneClient::col_as_address(row, "token1");
        let project = DuneClient::col_as_string(row, "project").unwrap_or_default().to_lowercase();
        let version = DuneClient::col_as_string(row, "version").unwrap_or_default().to_lowercase();
        let creation_block = DuneClient::col_as_u64(row, "creation_block").unwrap_or(0);

        let (addr, t0, t1) = match (address, token0, token1) {
            (Some(a), Some(t0), Some(t1)) if !seen.contains(&a) => (a, t0, t1),
            _ => continue,
        };
        if t0 == Address::ZERO || t1 == Address::ZERO || t0 == t1 {
            continue;
        }
        seen.insert(addr);

        let (dex_type, fee) = if version.contains("v3") || version == "3" {
            (DexType::UniswapV3, DuneClient::col_as_u64(row, "fee").unwrap_or(3000) as u32)
        } else if project.contains("curve") {
            (DexType::Curve, 0)
        } else if project.contains("balancer") {
            (DexType::Balancer, 0)
        } else if project.contains("dodo") {
            (DexType::Dodo, 0)
        } else if project.contains("clipper") {
            (DexType::Clipper, 0)
        } else if project.contains("solidly") || project.contains("velodrome")
            || project.contains("aerodrome") || project.contains("equalizer")
            || project.contains("thena") || project.contains(" Ramses")
        {
            (DexType::Solidly, 30)
        } else if project.contains("camelot") {
            (DexType::Camelot, 0)
        } else {
            (DexType::UniswapV2, DuneClient::col_as_u64(row, "fee").unwrap_or(30) as u32)
        };

        let ts = match dex_type {
            DexType::UniswapV3 => Some(tick_spacing_from_fee(fee)),
            _ => None,
        };

        pools.push(DiscoveredPool {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing: ts,
            dex_type,
            creation_block,
            pool_id: None,
            factory: None,
        });
    }

    tracing::info!(
        "Dune active pool discovery: found {} unique pools from QUERY_ALL_ACTIVE_POOLS",
        pools.len(),
    );
    Ok(pools)
}

/// Convert discovered pools into a lookup map by DexType for easy merging
/// with on-chain and RPC-discovered pools.
pub fn group_pools_by_dex_type(pools: Vec<DiscoveredPool>) -> HashMap<DexType, Vec<DiscoveredPool>> {
    let mut map = HashMap::new();
    for pool in pools {
        map.entry(pool.dex_type).or_insert_with(Vec::new).push(pool);
    }
    map
}

