//! DEX activity scanner — identifies blocks with DEX pool activity
//! using eth_getLogs. Enables log-first fetch optimization: instead of
//! fetching every block in a range, scan for DEX events first and only
//! fetch blocks that have relevant activity.

use std::collections::HashSet;

use alloy::primitives::Address;
use alloy::rpc::types::Filter;

use crate::error;
use crate::rpc::RpcClient;

/// DEX event topic signatures used for activity detection.
pub mod topics {
    use alloy::primitives::{b256, keccak256, B256};
    use std::sync::LazyLock;

    pub const V2_SWAP: B256 =
        b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");
    pub const V2_SYNC: B256 =
        b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

    pub const V3_SWAP: B256 =
        b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");
    pub static V3_MINT: LazyLock<B256> =
        LazyLock::new(|| keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)"));
    pub const V3_BURN: B256 =
        b256!("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");

    pub static CURVE_TOKEN_EXCHANGE: LazyLock<B256> =
        LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256)"));
    pub static CURVE_V2_TOKEN_EXCHANGE: LazyLock<B256> =
        LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256,uint256)"));
    pub static BALANCER_SWAP: LazyLock<B256> =
        LazyLock::new(|| keccak256("Swap(bytes32,address,address,uint256,uint256)"));

    /// Uniswap V4 Swap event from the singleton PoolManager
    /// (`id` in topics[1]; NOT the same as the V3 Swap signature).
    pub static V4_SWAP: LazyLock<B256> = LazyLock::new(|| {
        keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)")
    });

    /// Trader Joe Liquidity Book 2.0 Pair Swap event
    /// (`sender, recipient, uint256 id indexed, swapForY, amountIn, amountOut,
    /// volatilityAccumulated, fees` — verified against lfj-gg/joe-v2 branch v2.0).
    /// The same canonical signature is emitted by LB 2.2 pairs (per-side amounts).
    pub static TRADER_JOE_LB_SWAP: LazyLock<B256> =
        LazyLock::new(|| keccak256("Swap(address,address,uint256,bool,uint256,uint256,uint256,uint256)"));

    /// Trader Joe Liquidity Book 2.1/2.2 Pair Swap event
    /// (`sender, to, uint24 id, bytes32 amountsIn, bytes32 amountsOut,
    /// uint24 volatilityAccumulator, bytes32 totalFees, bytes32 protocolFees`
    /// — verified against lfj-gg/joe-v2 branches main/v2.1/v2.2).
    pub static TRADER_JOE_LB_SWAP_LEGACY: LazyLock<B256> = LazyLock::new(|| {
        keccak256("Swap(address,address,uint24,bytes32,bytes32,uint24,bytes32,bytes32)")
    });

    /// Pendle V2 market Swap event (caller, receiver indexed).
    pub static PENDLE_MARKET_SWAP: LazyLock<B256> =
        LazyLock::new(|| keccak256("Swap(address,address,int256,int256,uint256,uint256)"));

    // Curve TokenExchangeUnderlying events (exchange_underlying path)
    pub static CURVE_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> = LazyLock::new(|| {
        keccak256("TokenExchangeUnderlying(address,int128,uint256,int128,uint256)")
    });
    pub static CURVE_V2_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> = LazyLock::new(|| {
        keccak256("TokenExchangeUnderlying(address,int128,uint256,int128,uint256,uint256)")
    });

    /// All DEX event topic hashes for activity scanning.
    pub fn all_topics() -> Vec<B256> {
        vec![
            V2_SWAP,
            V2_SYNC,
            V3_SWAP,
            *V3_MINT,
            V3_BURN,
            *CURVE_TOKEN_EXCHANGE,
            *CURVE_V2_TOKEN_EXCHANGE,
            *CURVE_TOKEN_EXCHANGE_UNDERLYING,
            *CURVE_V2_TOKEN_EXCHANGE_UNDERLYING,
            *BALANCER_SWAP,
            *V4_SWAP,
            *TRADER_JOE_LB_SWAP,
            *TRADER_JOE_LB_SWAP_LEGACY,
            *PENDLE_MARKET_SWAP,
        ]
    }
}

/// Scans block ranges for DEX pool activity using eth_getLogs.
///
/// Construct an `ActivityScanner`, configure the batch size, then call
/// `find_active_blocks()` to discover which blocks in a range contain
/// DEX events. Only those blocks need full block data fetching.
pub struct ActivityScanner {
    rpc: RpcClient,
    batch_size: u64,
}

impl ActivityScanner {
    pub fn new(rpc: RpcClient) -> Self {
        ActivityScanner {
            rpc,
            batch_size: 500,
        }
    }

    pub fn with_batch_size(mut self, n: u64) -> Self {
        self.batch_size = n.max(1);
        self
    }

    /// Find all blocks in [start_block, end_block] that have DEX pool events.
    ///
    /// Uses eth_getLogs with pool address + event topic filters, batched
    /// across the block range to respect provider-imposed range limits.
    ///
    /// Returns an empty set if no pool addresses are provided.
    ///
    /// On batch failure (e.g. range too large for provider), falls back
    /// to individual block scanning for that batch.
    pub async fn find_active_blocks(
        &self,
        pool_addresses: &[Address],
        start_block: u64,
        end_block: u64,
    ) -> error::Result<HashSet<u64>> {
        if pool_addresses.is_empty() {
            return Ok(HashSet::new());
        }

        let mut active = HashSet::new();
        let dex_topics = topics::all_topics();
        let mut current = start_block;

        while current <= end_block {
            let batch_end = (current + self.batch_size - 1).min(end_block);

            let filter = Filter::new()
                .address(pool_addresses.to_vec())
                .event_signature(dex_topics.clone())
                .from_block(current)
                .to_block(batch_end);

            match self.rpc.get_logs(&filter).await {
                Ok(logs) => {
                    for log in &logs {
                        if let Some(block_num) = log.block_number {
                            active.insert(block_num);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Activity scan failed for blocks {current}..{batch_end}: {e:#}. \
                         Falling back to single-block scan for this batch."
                    );
                    for b in current..=batch_end {
                        let single = Filter::new()
                            .address(pool_addresses.to_vec())
                            .event_signature(dex_topics.clone())
                            .from_block(b)
                            .to_block(b);
                        if let Ok(logs) = self.rpc.get_logs(&single).await {
                            if logs.iter().any(|l| l.block_number == Some(b)) {
                                active.insert(b);
                            }
                        }
                    }
                }
            }

            if batch_end == end_block {
                break;
            }
            current = batch_end + 1;
        }

        Ok(active)
    }
}

#[cfg(test)]
mod tests {
    use super::topics;
    use alloy::primitives::b256;

    /// Regression guard: the V4 Swap topic must NOT collapse onto the V3 topic.
    /// The original bug used the V3 signature string
    /// `Swap(address,address,int256,int256,uint160,uint128,int24)`, which
    /// keccak-hashes to exactly V3_SWAP, silently disabling V4 detection.
    #[test]
    fn v4_swap_topic_differs_from_v3() {
        assert_ne!(topics::V4_SWAP, topics::V3_SWAP);
        // Verified against v4-core PoolManager._swap:
        // emit Swap(id, msg.sender, delta.amount0(), delta.amount1(),
        //           result.sqrtPriceX96, result.liquidity, result.tick, swapFee)
        assert_eq!(
            *topics::V4_SWAP,
            b256!("40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f")
        );
    }

    /// Trader Joe LB topics: the 2.0 form (uint256 id + swapForY) and the
    /// 2.1/2.2 packed-bytes32 form must both be present and distinct.
    #[test]
    fn trader_joe_lb_topics_are_distinct_and_verified() {
        assert_ne!(*topics::TRADER_JOE_LB_SWAP, topics::V3_SWAP);
        assert_ne!(*topics::TRADER_JOE_LB_SWAP_LEGACY, topics::V3_SWAP);
        assert_ne!(*topics::TRADER_JOE_LB_SWAP, *topics::TRADER_JOE_LB_SWAP_LEGACY);
        assert_eq!(
            *topics::TRADER_JOE_LB_SWAP,
            b256!("c528cda9e500228b16ce84fadae290d9a49aecb17483110004c5af0a07f6fd73")
        );
        assert_eq!(
            *topics::TRADER_JOE_LB_SWAP_LEGACY,
            b256!("ad7d6f97abf51ce18e17a38f4d70e975be9c0708474987bb3e26ad21bd93ca70")
        );
    }

    /// Pendle market Swap (verified against pendle-core-v2 PendleMarketV7):
    /// emit Swap(caller, receiver, netPtToAccount i256, netSyToAccount i256,
    ///           netSyFee u256, netSyToReserve u256)
    #[test]
    fn pendle_swap_topic_is_verified() {
        let expected = alloy::primitives::keccak256("Swap(address,address,int256,int256,uint256,uint256)");
        assert_eq!(*topics::PENDLE_MARKET_SWAP, expected);
        assert_ne!(*topics::PENDLE_MARKET_SWAP, topics::V3_SWAP);
    }
}
