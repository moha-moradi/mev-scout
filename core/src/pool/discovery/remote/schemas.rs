//! Schema-kind adapters: query generation + response parsing for each subgraph type.
//!
//! Each `SubgraphSchema` variant carries a `query()` + `parse()` pair. V4 reuses
//! the V3 shape (factory PoolManager events are not yet indexed by a dedicated
//! subgraph).

use alloy::primitives::Address;

use crate::dex_type::DexType;
use crate::types::SubgraphSchema;

use super::RemotePool;

// ── Helpers ───────────────────────────────────────────────────────────────

fn parse_address(s: &str) -> Option<Address> {
    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if s.len() != 40 { return None; }
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(s, &mut bytes).ok()?;
    Some(Address::from_slice(&bytes))
}

fn parse_f64_str(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn parse_u32_str(v: &serde_json::Value) -> Option<u32> {
    match v {
        serde_json::Value::String(s) => s.parse::<u32>().ok(),
        serde_json::Value::Number(n) => n.as_u64().map(|x| x as u32),
        _ => None,
    }
}

fn parse_i32_str(v: &serde_json::Value) -> Option<i32> {
    match v {
        serde_json::Value::String(s) => s.parse::<i32>().ok(),
        serde_json::Value::Number(n) => n.as_i64().map(|x| x as i32),
        _ => None,
    }
}

fn parse_u64_str(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::String(s) => s.parse::<u64>().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

// ── Query builder ───────────────────────────────────────────────────────

/// Build a GraphQL query string for the given schema, paginated by `first`/`skip`.
///
/// `min_tvl` is used to add a `where: { totalValueLockedUSD_gt: "..." }` filter
/// when the schema supports it (V3, V2, Algebra). The caller also does client-side
/// filtering as a safety net.
pub fn build_pools_query(schema: &SubgraphSchema, first: usize, skip: usize, min_tvl: Option<f64>) -> String {
    let tvl_where = min_tvl.map(|v| format!(", where: {{ totalValueLockedUSD_gt: \"{}\" }}", v as u64)).unwrap_or_default();
    let tvl_where_v2 = min_tvl.map(|v| format!(", where: {{ reserveUSD_gt: \"{}\" }}", v as u64)).unwrap_or_default();

    match schema {
        SubgraphSchema::UniswapV3 => format!(
            r#"{{ pools(first: {first}, skip: {skip}, orderBy: totalValueLockedUSD, orderDirection: desc{where}) {{ id token0 {{ id symbol }} token1 {{ id symbol }} feeTier tickSpacing totalValueLockedUSD volumeUSD txCount createdAtBlockNumber liquidity }} }}"#,
            where = tvl_where
        ),
        SubgraphSchema::Algebra => format!(
            r#"{{ pools(first: {first}, skip: {skip}, orderBy: totalValueLockedUSD, orderDirection: desc{where}) {{ id token0 {{ id symbol }} token1 {{ id symbol }} fee tickSpacing totalValueLockedUSD volumeUSD txCount createdAtBlockNumber liquidity }} }}"#,
            where = tvl_where
        ),
        SubgraphSchema::UniswapV2 => format!(
            r#"{{ pairs(first: {first}, skip: {skip}, orderBy: reserveUSD, orderDirection: desc{where}) {{ id token0 {{ id symbol }} token1 {{ id symbol }} reserveUSD volumeUSD txCount createdAtBlockNumber }} }}"#,
            where = tvl_where_v2
        ),
        SubgraphSchema::BalancerV2 => format!(
            r#"{{ pools(first: {first}, skip: {skip}, orderBy: totalLiquidity, orderDirection: desc) {{ id address tokens {{ address symbol }} totalLiquidity swapFee }} }}"#,
        ),
        SubgraphSchema::Curve => format!(
            r#"{{ pools(first: {first}, skip: {skip}, orderBy: tvl, orderDirection: desc) {{ id address coins tvl volumeUSD }} }}"#,
        ),
    }
}

// ── Parsers ───────────────────────────────────────────────────────────────

/// Parse a GraphQL response JSON into `RemotePool`s for the given schema.
///
/// `dex_type` and `dex_name` are passed in from the `SubgraphConfig` so that
/// the caller can override the schema default per DEX (e.g. Algebra → UniswapV3).
pub fn parse_pools(
    schema: &SubgraphSchema,
    json: &serde_json::Value,
    dex_type: DexType,
    dex_name: &str,
) -> anyhow::Result<Vec<RemotePool>> {
    match schema {
        SubgraphSchema::UniswapV3 => parse_uniswap_v3(json, dex_type, dex_name, false),
        SubgraphSchema::Algebra => parse_uniswap_v3(json, dex_type, dex_name, true),
        SubgraphSchema::UniswapV2 => parse_uniswap_v2(json, dex_type, dex_name),
        SubgraphSchema::BalancerV2 => parse_balancer_v2(json, dex_type, dex_name),
        SubgraphSchema::Curve => parse_curve(json, dex_type, dex_name),
    }
}

fn parse_uniswap_v3(
    json: &serde_json::Value,
    dex_type: DexType,
    dex_name: &str,
    is_algebra: bool,
) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").ok_or_else(|| anyhow::anyhow!("missing data field"))?;
    let pools = data.get("pools").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing pools array"))?;

    let mut out = Vec::new();
    for item in pools {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let addr = match parse_address(id) {
            Some(a) => a,
            None => { tracing::debug!("Skipping pool with invalid id {}", id); continue; }
        };
        let token0_obj = item.get("token0");
        let token1_obj = item.get("token1");

        // token0/token1 may be object { id, symbol } or string address
        let (t0, t0_sym) = extract_token(token0_obj);
        let (t1, t1_sym) = extract_token(token1_obj);
        let token0 = match t0 { Some(a) => a, None => continue };
        let token1 = match t1 { Some(a) => a, None => continue };

        // feeTier vs fee
        let fee = if is_algebra {
            item.get("fee").and_then(parse_u32_str).unwrap_or(3000)
        } else {
            item.get("feeTier").and_then(parse_u32_str)
                .or_else(|| item.get("fee").and_then(parse_u32_str))
                .unwrap_or(3000)
        };
        let tick_spacing = item.get("tickSpacing").and_then(parse_i32_str)
            .or_else(|| item.get("tickSpacing").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()));

        let tvl = item.get("totalValueLockedUSD").and_then(parse_f64_str);
        let volume = item.get("volumeUSD").and_then(parse_f64_str);
        let created = item.get("createdAtBlockNumber").and_then(parse_u64_str).unwrap_or(0);

        out.push(RemotePool {
            address: addr,
            token0,
            token1,
            fee,
            tick_spacing,
            dex_type,
            dex_name: Some(dex_name.to_string()),
            token0_symbol: t0_sym,
            token1_symbol: t1_sym,
            tvl_usd: tvl,
            volume_usd_24h: None,
            volume_usd_30d: volume, // use total volume as 30d fallback
            underlying_tokens: None,
            creation_block: created,
        });
    }
    Ok(out)
}

fn parse_uniswap_v2(
    json: &serde_json::Value,
    dex_type: DexType,
    dex_name: &str,
) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").ok_or_else(|| anyhow::anyhow!("missing data field"))?;
    // V2 subgraph uses `pairs` entity, not `pools`
    let pools = data.get("pairs").or_else(|| data.get("pools"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing pairs/pools array"))?;

    let mut out = Vec::new();
    for item in pools {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let addr = match parse_address(id) { Some(a) => a, None => continue };
        let (t0, t0_sym) = extract_token(item.get("token0"));
        let (t1, t1_sym) = extract_token(item.get("token1"));
        let token0 = match t0 { Some(a) => a, None => continue };
        let token1 = match t1 { Some(a) => a, None => continue };

        let tvl = item.get("reserveUSD").or_else(|| item.get("totalValueLockedUSD")).and_then(parse_f64_str)
            .or_else(|| item.get("reserveUSD").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()));
        let volume = item.get("volumeUSD").and_then(parse_f64_str);
        let created = item.get("createdAtBlockNumber").or_else(|| item.get("createdAtTimestamp"))
            .and_then(parse_u64_str).unwrap_or(0);

        // V2 fee defaults to 30 bps (caller may override via config v2_fee_override later)
        out.push(RemotePool {
            address: addr,
            token0,
            token1,
            fee: 30,
            tick_spacing: None,
            dex_type,
            dex_name: Some(dex_name.to_string()),
            token0_symbol: t0_sym,
            token1_symbol: t1_sym,
            tvl_usd: tvl,
            volume_usd_24h: None,
            volume_usd_30d: volume,
            underlying_tokens: None,
            creation_block: created,
        });
    }
    Ok(out)
}

fn parse_balancer_v2(
    json: &serde_json::Value,
    dex_type: DexType,
    dex_name: &str,
) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").ok_or_else(|| anyhow::anyhow!("missing data field"))?;
    let pools = data.get("pools").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing pools array"))?;

    let mut out = Vec::new();
    for item in pools {
        // Balancer uses `address` field for pool address; fallback to `id`
        let addr_str = item.get("address").or_else(|| item.get("id"))
            .and_then(|v| v.as_str()).unwrap_or("");
        let addr = match parse_address(addr_str) { Some(a) => a, None => continue };

        // tokens array: [{ address, symbol }, ...] or tokensList
        let tokens_val = item.get("tokens").or_else(|| item.get("tokensList"));
        let mut tokens: Vec<Address> = Vec::new();
        let mut symbols: Vec<String> = Vec::new();
        if let Some(arr) = tokens_val.and_then(|v| v.as_array()) {
            for t in arr {
                if let Some(obj) = t.as_object() {
                    let addr_s = obj.get("address").or_else(|| obj.get("id"))
                        .and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(a) = parse_address(addr_s) {
                        tokens.push(a);
                        if let Some(sym) = obj.get("symbol").and_then(|v| v.as_str()) {
                            symbols.push(sym.to_string());
                        }
                    }
                } else if let Some(s) = t.as_str() {
                    if let Some(a) = parse_address(s) { tokens.push(a); }
                }
            }
        }
        if tokens.len() < 2 { continue; }
        let token0 = tokens[0];
        let token1 = tokens[1];
        let t0_sym = symbols.get(0).cloned();
        let t1_sym = symbols.get(1).cloned();

        let tvl = item.get("totalLiquidity").and_then(parse_f64_str);
        let fee_str = item.get("swapFee").and_then(|v| v.as_str()).unwrap_or("0");
        // swapFee is decimal string like "0.003" → bps = fee * 10000
        let fee = fee_str.parse::<f64>().ok().map(|f| (f * 10000.0) as u32).unwrap_or(0);

        out.push(RemotePool {
            address: addr,
            token0,
            token1,
            fee,
            tick_spacing: None,
            dex_type,
            dex_name: Some(dex_name.to_string()),
            token0_symbol: t0_sym,
            token1_symbol: t1_sym,
            tvl_usd: tvl,
            volume_usd_24h: None,
            volume_usd_30d: None,
            underlying_tokens: Some(tokens),
            creation_block: 0,
        });
    }
    Ok(out)
}

fn parse_curve(
    json: &serde_json::Value,
    dex_type: DexType,
    dex_name: &str,
) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").ok_or_else(|| anyhow::anyhow!("missing data field"))?;
    let pools = data.get("pools").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing pools array"))?;

    let mut out = Vec::new();
    for item in pools {
        let addr_str = item.get("address").or_else(|| item.get("id"))
            .and_then(|v| v.as_str()).unwrap_or("");
        let addr = match parse_address(addr_str) { Some(a) => a, None => continue };

        // coins array
        let coins_val = item.get("coins").or_else(|| item.get("coinsList")).or_else(|| item.get("tokens"));
        let mut tokens: Vec<Address> = Vec::new();
        if let Some(arr) = coins_val.and_then(|v| v.as_array()) {
            for c in arr {
                if let Some(obj) = c.as_object() {
                    let s = obj.get("address").or_else(|| obj.get("id")).and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(a) = parse_address(s) { tokens.push(a); }
                } else if let Some(s) = c.as_str() {
                    if let Some(a) = parse_address(s) { tokens.push(a); }
                }
            }
        }
        if tokens.len() < 2 { continue; }
        let token0 = tokens[0];
        let token1 = tokens[1];
        let tvl = item.get("tvl").or_else(|| item.get("totalValueLockedUSD")).or_else(|| item.get("totalLiquidity"))
            .and_then(parse_f64_str);
        let vol = item.get("volumeUSD").and_then(parse_f64_str);

        out.push(RemotePool {
            address: addr,
            token0,
            token1,
            fee: 0,
            tick_spacing: None,
            dex_type,
            dex_name: Some(dex_name.to_string()),
            token0_symbol: None,
            token1_symbol: None,
            tvl_usd: tvl,
            volume_usd_24h: None,
            volume_usd_30d: vol,
            underlying_tokens: Some(tokens),
            creation_block: 0,
        });
    }
    Ok(out)
}

fn extract_token(val: Option<&serde_json::Value>) -> (Option<Address>, Option<String>) {
    let Some(v) = val else { return (None, None) };
    if let Some(s) = v.as_str() {
        return (parse_address(s), None);
    }
    if let Some(obj) = v.as_object() {
        let id = obj.get("id").or_else(|| obj.get("address")).and_then(|x| x.as_str()).unwrap_or("");
        let addr = parse_address(id);
        let sym = obj.get("symbol").and_then(|x| x.as_str()).map(|s| s.to_string());
        return (addr, sym);
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_query_uniswap_v3() {
        let q = build_pools_query(&SubgraphSchema::UniswapV3, 1000, 0, None);
        assert!(q.contains("pools"));
        assert!(q.contains("feeTier"));
        assert!(q.contains("first: 1000"));
    }

    #[test]
    fn test_build_query_algebra_has_fee() {
        let q = build_pools_query(&SubgraphSchema::Algebra, 500, 500, Some(1000.0));
        assert!(q.contains("fee"));
        assert!(q.contains("totalValueLockedUSD_gt"));
    }

    #[test]
    fn test_parse_uniswap_v3_single() {
        let json = json!({
            "data": {
                "pools": [{
                    "id": "0x1234567890123456789012345678901234567890",
                    "token0": {"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"},
                    "token1": {"id": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"},
                    "feeTier": "3000",
                    "tickSpacing": "60",
                    "totalValueLockedUSD": "12345.67",
                    "volumeUSD": "9876.54",
                    "createdAtBlockNumber": "49100001"
                }]
            }
        });
        let pools = parse_uniswap_v3(&json, DexType::UniswapV3, "Uniswap V3", false).unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].fee, 3000);
        assert_eq!(pools[0].tick_spacing, Some(60));
        assert_eq!(pools[0].tvl_usd, Some(12345.67));
        assert_eq!(pools[0].creation_block, 49100001);
        assert_eq!(pools[0].token0_symbol.as_deref(), Some("WETH"));
    }

    #[test]
    fn test_parse_uniswap_v3_algebra_fee_field() {
        let json = json!({
            "data": {
                "pools": [{
                    "id": "0x1234567890123456789012345678901234567890",
                    "token0": {"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"},
                    "token1": {"id": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"},
                    "fee": "500",
                    "tickSpacing": "10",
                    "totalValueLockedUSD": "5000",
                    "volumeUSD": "1000",
                    "createdAtBlockNumber": "49100002"
                }]
            }
        });
        let pools = parse_uniswap_v3(&json, DexType::UniswapV3, "Algebra", true).unwrap();
        assert_eq!(pools[0].fee, 500);
        assert_eq!(pools[0].tick_spacing, Some(10));
    }

    #[test]
    fn test_parse_uniswap_v2() {
        let json = json!({
            "data": {
                "pairs": [{
                    "id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "token0": {"id": "0x1111111111111111111111111111111111111111", "symbol": "WMATIC"},
                    "token1": {"id": "0x2222222222222222222222222222222222222222", "symbol": "USDC"},
                    "reserveUSD": "2000000",
                    "volumeUSD": "500000",
                    "createdAtBlockNumber": "50000000"
                }]
            }
        });
        let pools = parse_uniswap_v2(&json, DexType::UniswapV2, "QuickSwap V2").unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].fee, 30);
        assert_eq!(pools[0].tvl_usd, Some(2000000.0));
    }

    #[test]
    fn test_parse_balancer() {
        let json = json!({
            "data": {
                "pools": [{
                    "id": "0x123456789012345678901234567890123456789011223344556677889900aabb",
                    "address": "0x1234567890123456789012345678901234567890",
                    "tokens": [
                        {"address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WMATIC"},
                        {"address": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"}
                    ],
                    "totalLiquidity": "3000000",
                    "swapFee": "0.003"
                }]
            }
        });
        let pools = parse_balancer_v2(&json, DexType::Balancer, "Balancer").unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].fee, 30);
        assert!(pools[0].underlying_tokens.as_ref().unwrap().len() == 2);
    }

    #[test]
    fn test_parse_curve() {
        let json = json!({
            "data": {
                "pools": [{
                    "id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "coins": ["0x1111111111111111111111111111111111111111", "0x2222222222222222222222222222222222222222"],
                    "tvl": "10000000",
                    "volumeUSD": "200000"
                }]
            }
        });
        let pools = parse_curve(&json, DexType::Curve, "Curve").unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].underlying_tokens.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_extract_token_object() {
        let val = json!({"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"});
        let (addr, sym) = extract_token(Some(&val));
        assert!(addr.is_some());
        assert_eq!(sym.as_deref(), Some("WETH"));
    }
}
