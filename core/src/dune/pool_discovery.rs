use std::collections::HashMap;

use tracing;

use super::client::DuneClient;
use super::queries;
use crate::pool::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;

fn render_query(template: &str, chain: &str, from_block: u64, to_block: u64) -> String {
    template
        .replace("{chain}", &dune_chain_label(chain))
        .replace("{from_block}", &from_block.to_string())
        .replace("{to_block}", &to_block.to_string())
}

/// Map of chain names to DuneSQL chain labels.
/// Returns a `String` to handle non-static mappings (e.g. "avalanche" → "avalanche_c").
pub fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
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
        let creation_block = DuneClient::col_as_u64(row, "creation_block").unwrap_or(0);

        if let (Some(addr), Some(t0), Some(t1)) = (address, token0, token1) {
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
/// `project`, `project_type`, `last_active_block`, `fee`
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

        let (addr, t0, t1) = match (address, token0, token1) {
            (Some(a), Some(t0), Some(t1)) if !seen.contains(&a) => (a, t0, t1),
            _ => continue,
        };
        seen.insert(addr);

        let (dex_type, fee) = if project.contains("v3") {
            (DexType::UniswapV3, DuneClient::col_as_u64(row, "fee").unwrap_or(3000) as u32)
        } else if project.contains("curve") {
            (DexType::Curve, 0)
        } else if project.contains("balancer") {
            (DexType::Balancer, 0)
        } else {
            (DexType::UniswapV2, DuneClient::col_as_u64(row, "fee").unwrap_or(30) as u32)
        };

        pools.push(DiscoveredPool {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing: None,
            dex_type,
            creation_block: 0,
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

