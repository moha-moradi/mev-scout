use alloy::primitives::Address;
use tracing;

use super::client::DuneClient;
use super::pool_discovery::dune_chain_label;
use super::queries;
use super::types::DuneTokenWithStats;

/// Filter mode for token discovery.
#[derive(Debug, Clone)]
pub enum TokenFilter {
    /// All ERC-20 tokens on the chain (fast, no JOIN).
    All,
    /// Tokens with at least one DEX trade in the last N days.
    Active { days: u64 },
    /// Tokens first traded in the last N days.
    NewlyLaunched { days: u64 },
    /// Tokens ranked by estimated TVL (USD volume in active pools).
    Tvl { days: u64, top: usize },
}

/// Execute a token discovery query against Dune and return parsed results.
pub async fn discover_tokens(
    client: &DuneClient,
    chain: &str,
    filter: &TokenFilter,
    limit: usize,
) -> anyhow::Result<Vec<DuneTokenWithStats>> {
    let chain_label = dune_chain_label(chain);

    let sql = match filter {
        TokenFilter::All => {
            queries::QUERY_TOKENS_ALL
                .replace("{chain}", &chain_label)
                .replace("{limit}", &limit.to_string())
        }
        TokenFilter::Active { days } => {
            queries::QUERY_TOKENS_ACTIVE
                .replace("{chain}", &chain_label)
                .replace("{days}", &days.to_string())
                .replace("{limit}", &limit.to_string())
        }
        TokenFilter::NewlyLaunched { days } => {
            queries::QUERY_TOKENS_NEW
                .replace("{chain}", &chain_label)
                .replace("{days}", &days.to_string())
                .replace("{limit}", &limit.to_string())
        }
        TokenFilter::Tvl { days, top } => {
            queries::QUERY_TOKENS_TVL
                .replace("{chain}", &chain_label)
                .replace("{days}", &days.to_string())
                .replace("{limit}", &top.to_string())
        }
    };

    tracing::info!("Token discovery: executing Dune query (filter={:?})...", filter);
    let result = client.execute_raw_sql(&sql).await?;

    let rows = match result.result {
        Some(r) => r.rows,
        None => {
            tracing::warn!("Token discovery: Dune returned no results");
            return Ok(Vec::new());
        }
    };

    let mut tokens = Vec::with_capacity(rows.len());
    for row in &rows {
        let addr = match row.get("contract_address").and_then(|v| v.as_str()) {
            Some(s) => match s.parse::<Address>() {
                Ok(a) => a,
                Err(_) => continue,
            },
            None => continue,
        };
        let symbol = match row.get("symbol").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let decimals = row.get("decimals").and_then(|v| {
            if let Some(n) = v.as_u64() {
                Some(n as u8)
            } else if let Some(s) = v.as_str() {
                s.parse::<u8>().ok()
            } else {
                None
            }
        }).unwrap_or(18);

        let name = row.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());

        let trade_count = row.get("trade_count").and_then(|v| {
            if let Some(n) = v.as_u64() {
                Some(n)
            } else if let Some(s) = v.as_str() {
                s.parse::<u64>().ok()
            } else {
                None
            }
        });

        let volume_usd = row.get("volume_usd").and_then(|v| {
            if let Some(f) = v.as_f64() {
                Some(f)
            } else if let Some(s) = v.as_str() {
                s.parse::<f64>().ok()
            } else {
                None
            }
        });

        let tvl_usd = row.get("tvl_usd").and_then(|v| {
            if let Some(f) = v.as_f64() {
                Some(f)
            } else if let Some(s) = v.as_str() {
                s.parse::<f64>().ok()
            } else {
                None
            }
        });

        tokens.push(DuneTokenWithStats {
            contract_address: addr,
            symbol,
            decimals,
            name,
            trade_count,
            volume_usd,
            tvl_usd,
        });
    }

    tracing::info!("Token discovery: found {} tokens", tokens.len());
    Ok(tokens)
}

/// Post-filter tokens by symbol pattern (case-insensitive substring match).
pub fn filter_by_symbol(tokens: &mut Vec<DuneTokenWithStats>, pattern: &str) {
    let lower = pattern.to_lowercase();
    tokens.retain(|t| t.symbol.to_lowercase().contains(&lower));
}

/// Post-filter tokens by exact decimals value.
pub fn filter_by_decimals(tokens: &mut Vec<DuneTokenWithStats>, decimals: u8) {
    tokens.retain(|t| t.decimals == decimals);
}

/// Post-filter tokens by minimum USD trade volume.
pub fn filter_by_min_volume(tokens: &mut Vec<DuneTokenWithStats>, min_volume: f64) {
    tokens.retain(|t| t.volume_usd.unwrap_or(0.0) >= min_volume);
}

/// Sort tokens by a field name.
pub fn sort_by_field(tokens: &mut Vec<DuneTokenWithStats>, field: &str) {
    match field {
        "volume" => tokens.sort_by(|a, b| {
            b.volume_usd.unwrap_or(0.0).partial_cmp(&a.volume_usd.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "tvl" => tokens.sort_by(|a, b| {
            b.tvl_usd.unwrap_or(0.0).partial_cmp(&a.tvl_usd.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "trades" => tokens.sort_by(|a, b| {
            b.trade_count.unwrap_or(0).cmp(&a.trade_count.unwrap_or(0))
        }),
        "symbol" => tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol)),
        "name" => tokens.sort_by(|a, b| {
            a.name.as_deref().unwrap_or("").cmp(b.name.as_deref().unwrap_or(""))
        }),
        _ => {}
    }
}
