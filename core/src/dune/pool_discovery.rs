use std::collections::HashMap;

use tracing;

use super::client::DuneClient;
use crate::pool::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;

/// Map of chain names to DuneSQL chain labels.
/// Returns a `String` to handle non-static mappings (e.g. "avalanche" → "avalanche_c").
pub fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}

/// Discover V2-style pools by executing a Dune SQL query.
///
/// The caller must have created a Dune query (by ID) that returns columns:
/// `pool_address`, `token0`, `token1`, `creation_block`, `factory` (optional).
pub async fn discover_v2_pools_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
    fee_override: u32,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let dune_chain = dune_chain_label(chain);
    let from_str = from_block.to_string();
    let to_str = to_block.to_string();
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_str),
        ("to_block", &to_str),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;

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
        "Dune V2 discovery: found {} pools from query {}",
        pools.len(),
        query_id
    );
    Ok(pools)
}

/// Discover V3 pools via Dune.
///
/// Expected columns: `pool_address`, `token0`, `token1`, `fee`, `tick_spacing`,
/// `creation_block`, `factory` (optional)
pub async fn discover_v3_pools_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let dune_chain = dune_chain_label(chain);
    let from_str = from_block.to_string();
    let to_str = to_block.to_string();
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_str),
        ("to_block", &to_str),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;

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
        "Dune V3 discovery: found {} pools from query {}",
        pools.len(),
        query_id
    );
    Ok(pools)
}

/// Discover all active pools in a block range from `dex.trades` via Dune.
///
/// Expected Dune query columns: `pool_address`(0), `token0`(1), `token1`(2),
/// `project`(3), `project_type`(4), `last_active_block`(5), `fee`(6)
pub async fn discover_active_pools_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let dune_chain = dune_chain_label(chain);
    let from_str = from_block.to_string();
    let to_str = to_block.to_string();
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_str),
        ("to_block", &to_str),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;

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
        "Dune active pool discovery: found {} unique pools from query {}",
        pools.len(),
        query_id
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

