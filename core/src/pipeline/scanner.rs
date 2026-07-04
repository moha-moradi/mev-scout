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
    pub static V3_MINT: LazyLock<B256> = LazyLock::new(|| {
        keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)")
    });
    pub const V3_BURN: B256 =
        b256!("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");

    pub static CURVE_TOKEN_EXCHANGE: LazyLock<B256> =
        LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256)"));
    pub static CURVE_V2_TOKEN_EXCHANGE: LazyLock<B256> = LazyLock::new(|| {
        keccak256("TokenExchange(address,int128,uint256,int128,uint256,uint256)")
    });
    pub static BALANCER_SWAP: LazyLock<B256> =
        LazyLock::new(|| keccak256("Swap(bytes32,address,address,uint256,uint256)"));

    // Curve TokenExchangeUnderlying events (exchange_underlying path)
    pub static CURVE_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> =
        LazyLock::new(|| keccak256("TokenExchangeUnderlying(address,int128,uint256,int128,uint256)"));
    pub static CURVE_V2_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> =
        LazyLock::new(|| keccak256("TokenExchangeUnderlying(address,int128,uint256,int128,uint256,uint256)"));

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
            batch_size: 2000,
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
