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

/// Seconds per block for a chain (unrounded) — the single authoritative
/// timing table. Consumers used to keep their own `chain_id → seconds`
/// matches that disagreed with this one (Polygon 1.5s here vs 2s in the CLI,
/// BSC 3s here vs 1s there); route every timing query through here.
pub fn secs_per_block(chain: crate::types::ChainName) -> f64 {
    chain_timing(&chain.to_string()).secs_per_block
}

/// Whole seconds per block, rounding up — safe for coarse estimates.
pub fn block_time_secs(chain: crate::types::ChainName) -> u64 {
    (secs_per_block(chain).ceil() as u64).max(1)
}

/// Blocks per day for a chain, from the same authoritative table.
pub fn blocks_per_day(chain: crate::types::ChainName) -> u64 {
    chain_timing(&chain.to_string()).blocks_per_day
}

pub fn chain_timing(chain: &str) -> ChainTimingParams {
    match chain.to_lowercase().as_str() {
        "ethereum" => ChainTimingParams {
            genesis_ts: 1438269988,
            secs_per_block: 12.0,
            blocks_per_day: 7200,
            anchor_block: 0,
            anchor_ts: 0,
        },
        "polygon" => ChainTimingParams {
            genesis_ts: 1591031691,
            secs_per_block: 1.5,
            blocks_per_day: 57600,
            // Verified via polygon.drpc.org: head block 91370547 @ 2026-08-03 12:40:41 UTC.
            // Recent block rate measured at exactly 1.5 s/block across Jul 6 - Aug 3 2026.
            anchor_block: 91370547,
            anchor_ts: 1785760841,
        },
        "bsc" => ChainTimingParams {
            genesis_ts: 1597734000,
            secs_per_block: 3.0,
            blocks_per_day: 28800,
            anchor_block: 0,
            anchor_ts: 0,
        },
        "avalanche" | "avalanche_c" => ChainTimingParams {
            genesis_ts: 1624402800,
            secs_per_block: 2.0,
            blocks_per_day: 43200,
            anchor_block: 0,
            anchor_ts: 0,
        },
        "arbitrum" => ChainTimingParams {
            genesis_ts: 1630812600,
            secs_per_block: 0.26,
            blocks_per_day: 330000,
            anchor_block: 0,
            anchor_ts: 0,
        },
        "base" => ChainTimingParams {
            genesis_ts: 1686787200,
            secs_per_block: 2.0,
            blocks_per_day: 43200,
            anchor_block: 0,
            anchor_ts: 0,
        },
        "optimism" => ChainTimingParams {
            genesis_ts: 1631808000,
            secs_per_block: 2.0,
            blocks_per_day: 43200,
            anchor_block: 0,
            anchor_ts: 0,
        },
        _ => ChainTimingParams {
            genesis_ts: 1609459200,
            secs_per_block: 12.0,
            blocks_per_day: 7200,
            anchor_block: 0,
            anchor_ts: 0,
        },
    }
}
