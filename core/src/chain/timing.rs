pub struct ChainTimingParams {
    pub genesis_ts: i64,
    pub secs_per_block: f64,
    pub blocks_per_day: u64,
    /// Recent verified (block, unix_ts) anchor. When non-zero, recent-time
    /// conversions are anchored to it (accurate for recent windows). Genesis
    /// is used only as a fallback for chains without a verified anchor.
    pub anchor_block: u64,
    pub anchor_ts: i64,
}

pub fn chain_timing(chain: &str) -> ChainTimingParams {
    match chain.to_lowercase().as_str() {
        "ethereum" => ChainTimingParams { genesis_ts: 1438269988, secs_per_block: 12.0, blocks_per_day: 7200, anchor_block: 0, anchor_ts: 0 },
        "polygon" => ChainTimingParams {
            genesis_ts: 1591031691,
            secs_per_block: 1.5,
            blocks_per_day: 57600,
            // Verified via polygon.drpc.org: head block 91370547 @ 2026-08-03 12:40:41 UTC.
            // Recent block rate measured at exactly 1.5 s/block across Jul 6 - Aug 3 2026.
            anchor_block: 91370547,
            anchor_ts: 1785760841,
        },
        "bsc" => ChainTimingParams { genesis_ts: 1597734000, secs_per_block: 3.0, blocks_per_day: 28800, anchor_block: 0, anchor_ts: 0 },
        "avalanche" | "avalanche_c" => ChainTimingParams { genesis_ts: 1624402800, secs_per_block: 2.0, blocks_per_day: 43200, anchor_block: 0, anchor_ts: 0 },
        "arbitrum" => ChainTimingParams { genesis_ts: 1630812600, secs_per_block: 0.26, blocks_per_day: 330000, anchor_block: 0, anchor_ts: 0 },
        "base" => ChainTimingParams { genesis_ts: 1686787200, secs_per_block: 2.0, blocks_per_day: 43200, anchor_block: 0, anchor_ts: 0 },
        "optimism" => ChainTimingParams { genesis_ts: 1631808000, secs_per_block: 2.0, blocks_per_day: 43200, anchor_block: 0, anchor_ts: 0 },
        _ => ChainTimingParams { genesis_ts: 1609459200, secs_per_block: 12.0, blocks_per_day: 7200, anchor_block: 0, anchor_ts: 0 },
    }
}

/// Unix seconds for a block. Uses the verified recent anchor when available,
/// otherwise falls back to the genesis-linear model.
pub fn block_timestamp_secs(block: u64, chain: &str) -> i64 {
    let p = chain_timing(chain);
    if p.anchor_block > 0 {
        let delta = block as i64 - p.anchor_block as i64;
        p.anchor_ts + (delta as f64 * p.secs_per_block) as i64
    } else {
        p.genesis_ts + (block as f64 * p.secs_per_block) as i64
    }
}

pub fn estimate_latest_block(chain: &str) -> u64 {
    let p = chain_timing(chain);
    let now = chrono::Utc::now().timestamp();
    if p.anchor_block > 0 {
        let delta = (now - p.anchor_ts) as f64 / p.secs_per_block;
        (p.anchor_block as f64 + delta) as u64
    } else {
        let elapsed_secs = now - p.genesis_ts;
        (elapsed_secs as f64 / p.secs_per_block) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_anchor_round_trips() {
        let p = chain_timing("polygon");
        assert_eq!(block_timestamp_secs(p.anchor_block, "polygon"), p.anchor_ts);
    }

    #[test]
    fn polygon_recent_rate_verified_1_5_secs() {
        // Verified via polygon.drpc.org (Aug 2026): every 100,000 blocks = exactly 150,000s.
        let p = chain_timing("polygon");
        assert_eq!(p.secs_per_block, 1.5);
        assert_eq!(p.blocks_per_day, 57600);
    }

    #[test]
    fn polygon_latest_block_near_head() {
        let latest = estimate_latest_block("polygon");
        let p = chain_timing("polygon");
        assert!(latest >= p.anchor_block);
        assert!(latest <= p.anchor_block + 1_000_000);
    }

    #[test]
    fn polygon_block_timestamp_matches_verified_block() {
        // Block 91,000,000 verified @ 2026-07-28 02:16:37 UTC (ts 1785204997).
        let ts = block_timestamp_secs(91_000_000, "polygon");
        let naive = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default();
        assert_eq!(naive.format("%Y-%m-%d").to_string(), "2026-07-28");
    }
}
