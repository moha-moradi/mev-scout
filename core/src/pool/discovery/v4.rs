use super::V4_INITIALIZE_TOPIC;
use super::{DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::utils::abi_decode_address;
use alloy::primitives::Address;
use alloy::rpc::types::Filter;

/// V4 activity: swaps emit from the singleton PoolManager with a bytes32
/// poolId in topics[1]; the synthetic pool key is the poolId's last 20 bytes.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    if log.topics()[0] != *topics::V4_SWAP {
        return None;
    }
    let t = log.topics();
    if t.len() >= 2 {
        let mut pool_id = [0u8; 32];
        pool_id.copy_from_slice(t[1].as_slice());
        let pool_key = abi_decode_address(&pool_id, 0).unwrap_or(Address::ZERO);
        Some(PoolHitCandidate {
            pool_id: Some(pool_id),
            addr_override: Some(pool_key),
            ..PoolHitCandidate::simple(DexType::UniswapV4)
        })
    } else {
        Some(PoolHitCandidate::simple(DexType::UniswapV4))
    }
}

pub(crate) async fn scan_v4_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
    let mut out = ScanBatchResult::default();
    if let Some(pool_manager) = config.v4_pool_manager {
        let filter = Filter::new()
            .address(pool_manager)
            .event_signature(*V4_INITIALIZE_TOPIC)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        out.active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    let log_data = log.data();
                    if topics.len() < 4 || log_data.data.len() < 160 {
                        continue;
                    }
                    let Some(token0) = abi_decode_address(topics[2].as_slice(), 0) else {
                        continue;
                    };
                    let Some(token1) = abi_decode_address(topics[3].as_slice(), 0) else {
                        continue;
                    };
                    let fee = {
                        let mut fb = [0u8; 4];
                        fb[1] = log_data.data[29];
                        fb[2] = log_data.data[30];
                        fb[3] = log_data.data[31];
                        u32::from_be_bytes(fb)
                    };
                    let tick_spacing = {
                        let mut ts = [0u8; 4];
                        ts.copy_from_slice(&log_data.data[60..64]);
                        i32::from_be_bytes(ts)
                    };
                    // `hooks` is the third non-indexed ABI word (offset 64).
                    let hook_address =
                        abi_decode_address(&log_data.data, 64).filter(|a| !a.is_zero());
                    let creation_block = log.block_number.unwrap_or(0);
                    let Some(pool_addr) = abi_decode_address(topics[1].as_slice(), 0) else {
                        continue;
                    };
                    out.factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(
                            pool_addr,
                            token0,
                            token1,
                            fee,
                            DexType::UniswapV4,
                            creation_block,
                        )
                        .with_tick_spacing(Some(tick_spacing))
                        .with_factory(Some(pool_manager))
                        .with_hook_address(hook_address),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("V4 PoolManager scan failed for {current}..{batch_end}: {e:#}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_hook_is_the_third_abi_word() {
        let mut data = vec![0u8; 160];
        data[95] = 0xab;
        let hook = abi_decode_address(&data, 64).filter(|a| !a.is_zero());
        let mut expected = [0u8; 20];
        expected[19] = 0xab;
        assert_eq!(hook, Some(Address::from(expected)));
    }
}
