use super::scan_factory_creation_events_pinned;
use super::{DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use super::{ALGEBRA_POOL_CREATED_TOPIC, SLIPSTREAM_POOL_CREATED_TOPIC, V3_POOL_CREATED_TOPIC};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use alloy::primitives::Address;

/// V3 activity: per-pool contracts emit Swap/Mint/Burn from the pool address.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let t0 = log.topics()[0];
    if t0 == topics::V3_SWAP || t0 == *topics::V3_MINT || t0 == topics::V3_BURN {
        Some(PoolHitCandidate::simple(DexType::UniswapV3))
    } else {
        None
    }
}

pub(crate) async fn scan_v3_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
    if let Some(factories) = config.v3_factories {
        let mut out = ScanBatchResult::default();
        out.merge(
            scan_factory_creation_events_pinned(
                rpc,
                factories,
                *V3_POOL_CREATED_TOPIC,
                current,
                batch_end,
                provider_idx,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 64 || topics.len() < 4 {
                        return None;
                    }
                    let pool_addr = Address::from_slice(&log_data.data[44..64]);
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    let fee = u32::from_be_bytes([
                        topics[3][28],
                        topics[3][29],
                        topics[3][30],
                        topics[3][31],
                    ]);
                    let tick_spacing = {
                        let mut ts_bytes = [0u8; 4];
                        ts_bytes.copy_from_slice(&log_data.data[28..32]);
                        Some(i32::from_be_bytes(ts_bytes))
                    };
                    let creation_block = log.block_number.unwrap_or(0);
                    Some((
                        pool_addr,
                        DiscoveredPool::new(
                            pool_addr,
                            token0,
                            token1,
                            fee,
                            DexType::UniswapV3,
                            creation_block,
                        )
                        .with_tick_spacing(tick_spacing)
                        .with_factory(Some(log.address())),
                    ))
                },
            )
            .await,
        );
        out.merge(
            scan_factory_creation_events_pinned(
                rpc,
                factories,
                *ALGEBRA_POOL_CREATED_TOPIC,
                current,
                batch_end,
                provider_idx,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if topics.len() < 3 {
                        return None;
                    }
                    if log_data.data.len() < 32 {
                        return None;
                    }
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    // pool address is ABI-encoded address in log data (32 bytes, last 20 bytes)
                    let pool_addr = Address::from_slice(&log_data.data[12..32]);
                    if token0.is_zero() || token1.is_zero() || pool_addr.is_zero() {
                        return None;
                    }
                    let creation_block = log.block_number.unwrap_or(0);
                    // fee / tickSpacing not in event; will be fetched via eth_call in Phase 2.
                    Some((
                        pool_addr,
                        DiscoveredPool::new(
                            pool_addr,
                            token0,
                            token1,
                            0,
                            DexType::UniswapV3,
                            creation_block,
                        )
                        .with_factory(Some(log.address()))
                        .with_dex_name(Some("QuickSwap Algebra".to_string())),
                    ))
                },
            )
            .await,
        );
        out.merge(
            scan_factory_creation_events_pinned(
                rpc,
                factories,
                *SLIPSTREAM_POOL_CREATED_TOPIC,
                current,
                batch_end,
                provider_idx,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 32 || topics.len() < 4 {
                        return None;
                    }
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    // int24 tickSpacing is right-aligned in the third topic's 32-byte word.
                    let tick_spacing = {
                        let ts_bytes = [topics[3][29], topics[3][30], topics[3][31]];
                        let sign = if ts_bytes[0] & 0x80 != 0 { 0xFF } else { 0 };
                        i32::from_be_bytes([sign, ts_bytes[0], ts_bytes[1], ts_bytes[2]])
                    };
                    // pool address is ABI-encoded address in log data (32 bytes, last 20 bytes)
                    let pool_addr = Address::from_slice(&log_data.data[12..32]);
                    if token0.is_zero() || token1.is_zero() || pool_addr.is_zero() {
                        return None;
                    }
                    let creation_block = log.block_number.unwrap_or(0);
                    // fee / liquidity not in event; CL pools expose slot0/liquidity so
                    // V3 state init applies, fee defaults to 0 (repaired via eth_call in Phase 2).
                    Some((
                        pool_addr,
                        DiscoveredPool::new(
                            pool_addr,
                            token0,
                            token1,
                            0,
                            DexType::UniswapV3,
                            creation_block,
                        )
                        .with_tick_spacing(Some(tick_spacing))
                        .with_factory(Some(log.address()))
                        .with_dex_name(Some("Slipstream CL".to_string())),
                    ))
                },
            )
            .await,
        );
        return out;
    }
    ScanBatchResult::default()
}
