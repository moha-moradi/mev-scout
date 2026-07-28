pub struct ChainTimingParams {
    pub genesis_ts: i64,
    pub secs_per_block: f64,
    pub blocks_per_day: u64,
    pub dune_lag_secs: i64,
}

pub fn chain_timing(chain: &str) -> ChainTimingParams {
    match chain.to_lowercase().as_str() {
        "ethereum" => ChainTimingParams { genesis_ts: 1438269988, secs_per_block: 12.0, blocks_per_day: 7200, dune_lag_secs: 60 * 24 * 3600 },
        "polygon" => ChainTimingParams { genesis_ts: 1591031691, secs_per_block: 2.1, blocks_per_day: 41000, dune_lag_secs: 60 * 24 * 3600 },
        "bsc" => ChainTimingParams { genesis_ts: 1597734000, secs_per_block: 3.0, blocks_per_day: 28800, dune_lag_secs: 60 * 24 * 3600 },
        "avalanche" | "avalanche_c" => ChainTimingParams { genesis_ts: 1624402800, secs_per_block: 2.0, blocks_per_day: 43200, dune_lag_secs: 60 * 24 * 3600 },
        "arbitrum" => ChainTimingParams { genesis_ts: 1630812600, secs_per_block: 0.26, blocks_per_day: 330000, dune_lag_secs: 60 * 24 * 3600 },
        "base" => ChainTimingParams { genesis_ts: 1686787200, secs_per_block: 2.0, blocks_per_day: 43200, dune_lag_secs: 60 * 24 * 3600 },
        "optimism" => ChainTimingParams { genesis_ts: 1631808000, secs_per_block: 2.0, blocks_per_day: 43200, dune_lag_secs: 60 * 24 * 3600 },
        _ => ChainTimingParams { genesis_ts: 1609459200, secs_per_block: 12.0, blocks_per_day: 7200, dune_lag_secs: 60 * 24 * 3600 },
    }
}

pub fn estimate_latest_block(chain: &str) -> u64 {
    let p = chain_timing(chain);
    let now = chrono::Utc::now().timestamp();
    let elapsed_secs = now - p.genesis_ts;
    (elapsed_secs as f64 / p.secs_per_block) as u64
}

pub fn dune_indexing_lag_blocks(chain: &str) -> u64 {
    let p = chain_timing(chain);
    (p.dune_lag_secs as f64 / p.secs_per_block) as u64
}

/// Map of chain names to DuneSQL chain labels.
/// Returns a `String` to handle non-static mappings (e.g. "avalanche" → "avalanche_c").
pub fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}

pub fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let p = chain_timing(chain);
    let elapsed = block_number as f64 * p.secs_per_block;
    let approx_ts = p.genesis_ts + elapsed as i64;
    let naive = chrono::DateTime::from_timestamp(approx_ts, 0)
        .unwrap_or_default();
    naive.format("%Y-%m-%d").to_string()
}

pub fn render_query(template: &str, chain: &str, from_block: u64, to_block: u64) -> String {
    let chain_label = dune_chain_label(chain);
    let block_month_min = approx_block_month_min(from_block, &chain_label);
    template
        .replace("{chain}", &chain_label)
        .replace("{block_month_min}", &block_month_min)
        .replace("{from_block}", &from_block.to_string())
        .replace("{to_block}", &to_block.to_string())
}
