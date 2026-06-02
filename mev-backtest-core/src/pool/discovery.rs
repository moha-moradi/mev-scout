use std::collections::HashSet;
use std::path::PathBuf;

use alloy::primitives::{b256, Address, B256};
use alloy::rpc::types::Filter;

use crate::pool::state::PoolInfo;
use crate::rpc::RpcClient;

/// PairCreated(address indexed token0, address indexed token1, address pair, uint256)
const PAIR_CREATED_TOPIC: B256 =
    b256!("0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9");

const DEFAULT_CHUNK_SIZE: u64 = 100_000;

/// Discovers Uniswap V2 pools by scanning PairCreated events on-chain.
pub struct PoolDiscoverer {
    rpc: RpcClient,
    cursor_dir: Option<PathBuf>,
}

impl PoolDiscoverer {
    pub fn new(rpc: RpcClient) -> Self {
        PoolDiscoverer {
            rpc,
            cursor_dir: None,
        }
    }

    pub fn with_cursor_dir(mut self, dir: PathBuf) -> Self {
        self.cursor_dir = Some(dir);
        self
    }

    /// Scan all configured factories from `start_block` to `to_block`, returning newly
    /// discovered pools that are not already in `existing`.
    pub async fn discover_new_pools(
        &self,
        factories: &[Address],
        start_block: u64,
        to_block: u64,
        existing: &HashSet<Address>,
    ) -> anyhow::Result<Vec<PoolInfo>> {
        let mut all_pools = Vec::new();
        let mut seen = existing.clone();

        for &factory in factories {
            let mut cursor = self.load_cursor(factory).unwrap_or(start_block);
            while cursor <= to_block {
                let end = (cursor + DEFAULT_CHUNK_SIZE - 1).min(to_block);
                let pools = self.scan_factory_chunk(factory, cursor, end).await?;

                for pool in pools {
                    if seen.insert(pool.address) {
                        all_pools.push(pool);
                    }
                }

                cursor = end + 1;
                self.save_cursor(factory, cursor)?;
            }
        }

        Ok(all_pools)
    }

    async fn scan_factory_chunk(
        &self,
        factory: Address,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<PoolInfo>> {
        let filter = Filter::new()
            .from_block(from_block)
            .to_block(to_block)
            .address(factory)
            .event_signature(PAIR_CREATED_TOPIC);

        let logs = self.rpc.get_logs(&filter).await?;

        let mut pools = Vec::with_capacity(logs.len());
        for log in logs {
            if log.topics().len() < 3 {
                continue;
            }
            let token0 = Address::from_slice(&log.topics()[1].as_slice()[12..]);
            let token1 = Address::from_slice(&log.topics()[2].as_slice()[12..]);

            if log.data().data.len() < 32 {
                continue;
            }
            let pair_addr = Address::from_slice(&log.data().data[12..32]);

            pools.push(PoolInfo {
                address: pair_addr,
                pool_type: "UniswapV2".to_string(),
                token0,
                token1,
                fee: 30,
                name: None,
                dex_type: Default::default(),
                tick_spacing: None,
            });
        }

        Ok(pools)
    }

    fn load_cursor(&self, factory: Address) -> Option<u64> {
        let path = self.cursor_path(factory)?;
        let content = std::fs::read_to_string(&path).ok()?;
        content.trim().parse::<u64>().ok()
    }

    fn save_cursor(&self, factory: Address, block: u64) -> anyhow::Result<()> {
        if let Some(path) = self.cursor_path(factory) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, block.to_string())?;
        }
        Ok(())
    }

    fn cursor_path(&self, factory: Address) -> Option<PathBuf> {
        self.cursor_dir
            .as_ref()
            .map(|dir| dir.join(format!("discovery_cursor_{}.txt", factory)))
    }
}
