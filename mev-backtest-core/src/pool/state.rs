use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use alloy::primitives::{b256, keccak256, Address, Bytes, B256, U256};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::data::ExecutedLog;
use crate::pool::decoders;
use crate::pool::dex_type::DexType;
use crate::rpc::RpcClient;

/// Static pool information loaded from the registry JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub dex_type: DexType,
    #[serde(default)]
    pub tick_spacing: Option<u32>,
}

impl PoolInfo {
    pub fn is_concentrated_liquidity(&self) -> bool {
        self.dex_type.is_concentrated_liquidity()
    }
}

/// Runtime state for a Uniswap V2 constant-product pool.
#[derive(Debug, Clone)]
pub struct UniswapV2PoolState {
    pub info: PoolInfo,
    pub reserve0: u128,
    pub reserve1: u128,
}

/// Runtime state for a Uniswap V3 concentrated-liquidity pool.
#[derive(Debug, Clone)]
pub struct UniswapV3PoolState {
    pub info: PoolInfo,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    /// Ticks with position liquidity (tick_idx → net_liquidity_delta from all positions)
    pub ticks: HashMap<i32, i128>,
}

impl UniswapV3PoolState {
    pub fn new(info: PoolInfo) -> Self {
        UniswapV3PoolState {
            info,
            sqrt_price_x96: U256::ZERO,
            tick: 0,
            liquidity: 0,
            ticks: HashMap::new(),
        }
    }
}

/// Runtime state for a Curve pool (stable-swap / crypto).
#[derive(Debug, Clone)]
pub struct CurvePoolState {
    pub info: PoolInfo,
    pub balances: Vec<u128>,
    pub token_index: HashMap<Address, usize>,
}

/// Runtime state for a Balancer V2 weighted/stable pool.
#[derive(Debug, Clone)]
pub struct BalancerPoolState {
    pub info: PoolInfo,
    pub balances: Vec<u128>,
    pub token_index: HashMap<Address, usize>,
    pub pool_id: Option<[u8; 32]>,
}

/// Runtime state for any tracked pool.
#[derive(Debug, Clone)]
pub enum PoolState {
    UniswapV2(UniswapV2PoolState),
    UniswapV3(UniswapV3PoolState),
    Curve(CurvePoolState),
    Balancer(BalancerPoolState),
}

impl PoolState {
    pub fn address(&self) -> Address {
        match self {
            PoolState::UniswapV2(s) => s.info.address,
            PoolState::UniswapV3(s) => s.info.address,
            PoolState::Curve(s) => s.info.address,
            PoolState::Balancer(s) => s.info.address,
        }
    }

    pub fn info(&self) -> &PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
        }
    }

    pub fn info_mut(&mut self) -> &mut PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &mut s.info,
            PoolState::UniswapV3(s) => &mut s.info,
            PoolState::Curve(s) => &mut s.info,
            PoolState::Balancer(s) => &mut s.info,
        }
    }

    pub fn dex_label(&self) -> &'static str {
        self.info().dex_type.label()
    }
}

/// Internal helper: result of fetching on-chain state for a pool during init.
enum PoolInitResult {
    V2Reserves(u128, u128),
    V3State(U256, i32, u128),
}

/// Event signature for Uniswap V2 Swap event
const SWAP_TOPIC: B256 = b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");
/// Event signature for Uniswap V2 Sync event
const SYNC_TOPIC: B256 = b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");
/// getReserves() selector
const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// slot0() selector for Uniswap V3
static V3_SLOT0_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"slot0()");
    Bytes::copy_from_slice(&hash[..4])
});
/// liquidity() selector for Uniswap V3
static V3_LIQUIDITY_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"liquidity()");
    Bytes::copy_from_slice(&hash[..4])
});

/// Manages runtime pool state: initializes from RPC, updates from Swap/Sync events.
#[derive(Debug, Clone)]
pub struct PoolManager {
    pools: HashMap<Address, PoolState>,
    /// token address -> list of pool addresses that trade this token
    token_index: HashMap<Address, Vec<Address>>,
    /// Cached arbitrage pairs (invalidated on add_pool)
    pairs_cache: RefCell<Option<Vec<(Address, Address, Address)>>>,
}

impl PoolManager {
    pub fn new() -> Self {
        PoolManager {
            pools: HashMap::new(),
            token_index: HashMap::new(),
            pairs_cache: RefCell::new(None),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        PoolManager {
            pools: HashMap::with_capacity(capacity),
            token_index: HashMap::with_capacity(capacity),
            pairs_cache: RefCell::new(None),
        }
    }

    pub fn add_pool(&mut self, state: PoolState) {
        let addr = state.address();
        let info = state.info().clone();
        self.pools.insert(addr, state);
        self.token_index
            .entry(info.token0)
            .or_default()
            .push(addr);
        self.token_index
            .entry(info.token1)
            .or_default()
            .push(addr);
        *self.pairs_cache.borrow_mut() = None; // invalidate cache
    }

    pub fn get(&self, address: &Address) -> Option<&PoolState> {
        self.pools.get(address)
    }

    pub fn get_mut(&mut self, address: &Address) -> Option<&mut PoolState> {
        self.pools.get_mut(address)
    }

    pub fn all_pools(&self) -> impl Iterator<Item = &PoolState> {
        self.pools.values()
    }

    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    pub fn pool_addresses(&self) -> Vec<Address> {
        self.pools.keys().copied().collect()
    }

    /// Returns all unique token addresses tracked by the pool manager.
    pub fn token_addresses(&self) -> Vec<Address> {
        self.token_index.keys().copied().collect()
    }

    /// Returns all pool addresses that trade the given token.
    pub fn pools_for_token(&self, token: &Address) -> Option<&[Address]> {
        self.token_index.get(token).map(|v| v.as_slice())
    }

    /// Find a pool that trades both token_a and token_b (typically WMATIC pair discovery).
    pub fn find_pair_pool(&self, token_a: &Address, token_b: &Address) -> Option<Address> {
        let pools_a = self.token_index.get(token_a)?;
        let pools_b = self.token_index.get(token_b)?;
        // Find the first address common to both sets
        // Use the smaller set for iteration
        let (smaller, larger) = if pools_a.len() < pools_b.len() {
            (pools_a, pools_b)
        } else {
            (pools_b, pools_a)
        };
        smaller.iter().find(|addr| larger.contains(addr)).copied()
    }

    /// Returns pairs of pool addresses that share at least one common token.
    /// Each pair is returned once (pool_a < pool_b by address), with the shared token.
    /// Result is cached and invalidated on add_pool.
    pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
        if let Some(cached) = &*self.pairs_cache.borrow() {
            return cached.clone();
        }
        let mut pairs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (_token, pool_addrs) in &self.token_index {
            for i in 0..pool_addrs.len() {
                for j in (i + 1)..pool_addrs.len() {
                    let a = pool_addrs[i];
                    let b = pool_addrs[j];
                    let key = if a < b { (a, b) } else { (b, a) };
                    if seen.insert(key) {
                        pairs.push((key.0, key.1, *_token));
                    }
                }
            }
        }

        *self.pairs_cache.borrow_mut() = Some(pairs.clone());
        pairs
    }

    /// Update a V2 pool's reserves using amounts from a Swap event.
    pub fn apply_v2_swap(
        &mut self,
        address: &Address,
        amount0_in: u128,
        amount1_in: u128,
        amount0_out: u128,
        amount1_out: u128,
    ) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = state.reserve0.wrapping_add(amount0_in).wrapping_sub(amount0_out);
            state.reserve1 = state.reserve1.wrapping_add(amount1_in).wrapping_sub(amount1_out);
        }
    }

    /// Update a V2 pool's reserves from a Sync event (authoritative override).
    pub fn apply_v2_sync(&mut self, address: &Address, reserve0: u128, reserve1: u128) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = reserve0;
            state.reserve1 = reserve1;
        }
    }

    /// Update a V3 pool's state from a Swap event.
    pub fn apply_v3_swap(
        &mut self,
        address: &Address,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            state.sqrt_price_x96 = sqrt_price_x96;
            state.tick = tick;
            state.liquidity = liquidity;
        }
    }

    /// Update a V3 pool's tick liquidity from a Mint or Burn event.
    pub fn apply_v3_mint_burn(
        &mut self,
        address: &Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: i128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            *state.ticks.entry(tick_lower).or_insert(0) += amount;
            *state.ticks.entry(tick_upper).or_insert(0) -= amount;
            if amount > 0 {
                state.liquidity = state.liquidity.saturating_add(amount as u128);
            } else {
                state.liquidity = state.liquidity.saturating_sub((-amount) as u128);
            }
        }
    }

    /// Count pools that have non-zero reserves (i.e., initialized).
    pub fn initialized_count(&self) -> usize {
        self.pools.values().filter(|p| match p {
            PoolState::UniswapV2(s) => s.reserve0 > 0 && s.reserve1 > 0,
            PoolState::UniswapV3(s) => s.liquidity > 0,
            PoolState::Curve(s) => s.balances.iter().all(|b| *b > 0),
            PoolState::Balancer(s) => s.balances.iter().all(|b| *b > 0),
        })
        .count()
    }

    /// Initialize pool state from on-chain calls at a historical block.
    /// Dispatches V2 / V3 / Curve / Balancer per pool type.
    /// Fetches all pools in parallel, capped at 20 concurrent calls.
    pub async fn init_from_rpc(&mut self, rpc: &RpcClient, block_num: u64) {
        let pool_addrs: Vec<Address> = self.pools.keys().copied().collect();
        let cap = pool_addrs.len().clamp(1, 20);
        let semaphore = Arc::new(Semaphore::new(cap));

        // Snapshot dex types before spawning tasks
        let dex_types: Vec<DexType> = pool_addrs
            .iter()
            .map(|addr| match self.pools.get(addr) {
                Some(PoolState::UniswapV2(_)) => DexType::UniswapV2,
                Some(PoolState::UniswapV3(_)) => DexType::UniswapV3,
                Some(PoolState::Curve(_)) => DexType::Curve,
                Some(PoolState::Balancer(_)) => DexType::Balancer,
                None => DexType::UniswapV2,
            })
            .collect();

        let tasks: Vec<_> = pool_addrs
            .iter()
            .zip(dex_types.iter())
            .map(|(addr, dt)| {
                let rpc = rpc.clone();
                let sem = Arc::clone(&semaphore);
                let addr = *addr;
                let dt = *dt;
                async move {
                    let _permit = sem.acquire_owned().await.ok();
                    Self::fetch_pool_state(&rpc, addr, dt, block_num).await
                }
            })
            .collect();

        let results = join_all(tasks).await;

        for (addr, result) in pool_addrs.iter().zip(results) {
            match result {
                Some(PoolInitResult::V2Reserves(r0, r1)) => {
                    if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(addr) {
                        state.reserve0 = r0;
                        state.reserve1 = r1;
                    }
                }
                Some(PoolInitResult::V3State(sqrt, tick, liq)) => {
                    if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(addr) {
                        state.sqrt_price_x96 = sqrt;
                        state.tick = tick;
                        state.liquidity = liq;
                    }
                }
                None => {
                    tracing::warn!("Failed to fetch state for pool {}", addr);
                }
            }
        }
    }

    /// Fetch the appropriate on-chain state for a pool based on its type.
    async fn fetch_pool_state(
        rpc: &RpcClient,
        pool: Address,
        dt: DexType,
        block: u64,
    ) -> Option<PoolInitResult> {
        match dt {
            DexType::UniswapV2 => {
                let (r0, r1) = Self::fetch_v2_reserves(rpc, pool, block).await?;
                Some(PoolInitResult::V2Reserves(r0, r1))
            }
            DexType::UniswapV3 => {
                let (sqrt, tick, liq) = Self::fetch_v3_state(rpc, pool, block).await?;
                Some(PoolInitResult::V3State(sqrt, tick, liq))
            }
            DexType::Curve | DexType::Balancer => {
                // Curve & Balancer state depends on pool-specific parameters
                // (A, gamma, weights, etc.) — not yet implemented
                None
            }
        }
    }

    async fn fetch_v2_reserves(rpc: &RpcClient, pool: Address, block: u64) -> Option<(u128, u128)> {
        // Try eth_call getReserves() first
        let data = Bytes::copy_from_slice(&GET_RESERVES_SELECTOR);
        if let Ok(result) = rpc.call(pool, data, block).await {
            if result.len() >= 64 {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(&result[..32]);
                let r0 = U256::from_be_bytes(buf).as_limbs()[0] as u128;
                buf.copy_from_slice(&result[32..64]);
                let r1 = U256::from_be_bytes(buf).as_limbs()[0] as u128;
                return Some((r0, r1));
            }
        }
        tracing::trace!("eth_call getReserves() failed, falling back to storage for {}", pool);
        Self::fetch_v2_reserves_storage(rpc, pool, block).await
    }

    /// Given raw slot 6 (packed uint112 reserve0 | uint112 reserve1 | uint32 blockTimestampLast),
    /// decode reserve0 and reserve1.
    fn decode_v2_reserves_from_storage(raw: U256) -> (u128, u128) {
        let bytes = raw.to_be_bytes::<32>();
        let r0 = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf[2..16].copy_from_slice(&bytes[18..32]);
            buf
        });
        let r1 = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf[2..16].copy_from_slice(&bytes[4..18]);
            buf
        });
        (r0, r1)
    }

    /// Fallback: fetch V2 reserves via eth_getStorageAt slot 6.
    async fn fetch_v2_reserves_storage(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<(u128, u128)> {
        let raw = rpc.get_storage_at(pool, U256::from(6), block).await.ok()?;
        Some(Self::decode_v2_reserves_from_storage(raw))
    }

    /// Fetch V3 pool slot0() + liquidity() at a historical block.
    async fn fetch_v3_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<(U256, i32, u128)> {
        let slot0_result = rpc.call(pool, V3_SLOT0_SELECTOR.clone(), block).await;
        let liq_result = rpc.call(pool, V3_LIQUIDITY_SELECTOR.clone(), block).await;
        if let (Ok(slot0), Ok(liq)) = (slot0_result.as_ref(), liq_result.as_ref()) {
            if slot0.len() >= 96 && liq.len() >= 32 {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(&slot0[..32]);
                let sqrt_price_x96 = U256::from_be_bytes(buf);
                let mut tick_bytes = [0u8; 4];
                tick_bytes.copy_from_slice(&slot0[60..64]);
                let tick = i32::from_be_bytes(tick_bytes);
                buf.copy_from_slice(&liq[..32]);
                let liquidity = U256::from_be_bytes(buf).as_limbs()[0] as u128;
                return Some((sqrt_price_x96, tick, liquidity));
            }
        }
        if slot0_result.is_err() {
            tracing::trace!("eth_call slot0() failed for {}", pool);
        }
        if liq_result.is_err() {
            tracing::trace!("eth_call liquidity() failed for {}", pool);
        }
        if let Ok(slot0) = slot0_result.as_ref() {
            if slot0.len() < 96 {
                tracing::trace!("eth_call slot0() returned short result for {}", pool);
            }
        }
        if let Ok(liq) = liq_result.as_ref() {
            if liq.len() < 32 {
                tracing::trace!("eth_call liquidity() returned short result for {}", pool);
            }
        }
        tracing::trace!("falling back to storage for V3 pool {}", pool);
        Self::fetch_v3_state_storage(rpc, pool, block).await
    }

    /// Given raw slot0 + slot1, decode sqrtPriceX96, tick, and liquidity.
    fn decode_v3_state_from_storage(slot0_raw: U256, slot1_raw: U256) -> (U256, i32, u128) {
        let bytes = slot0_raw.to_be_bytes::<32>();
        let sqrt_price_x96 = U256::from_be_bytes({
            let mut buf = [0u8; 32];
            buf[12..32].copy_from_slice(&bytes[12..32]);
            buf
        });
        let mut tick_buf = [0u8; 4];
        tick_buf[1..4].copy_from_slice(&bytes[9..12]);
        if tick_buf[1] & 0x80 != 0 {
            tick_buf[0] = 0xFF;
        }
        let tick = i32::from_be_bytes(tick_buf);

        let bytes = slot1_raw.to_be_bytes::<32>();
        let liquidity = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&bytes[16..32]);
            buf
        });

        (sqrt_price_x96, tick, liquidity)
    }

    /// Fallback: fetch V3 state via eth_getStorageAt.
    async fn fetch_v3_state_storage(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<(U256, i32, u128)> {
        let slot0_raw = rpc.get_storage_at(pool, U256::ZERO, block).await.ok()?;
        let slot1_raw = rpc.get_storage_at(pool, U256::from(1), block).await.ok()?;
        Some(Self::decode_v3_state_from_storage(slot0_raw, slot1_raw))
    }

    /// Process a list of executed logs from a single transaction, updating pool state
    /// for any Swap or Sync events emitted by tracked pools.
    pub fn update_from_logs(&mut self, logs: &[ExecutedLog]) {
        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            let topic0 = log.topics[0];

            // V2 Swap
            if topic0 == SWAP_TOPIC {
                self.process_v2_swap_log(log);
                continue;
            }
            // V2 Sync
            if topic0 == SYNC_TOPIC {
                self.process_v2_sync_log(log);
                continue;
            }
            // V3 Swap
            if topic0 == decoders::V3_SWAP_TOPIC {
                self.process_v3_swap_log(log);
                continue;
            }
            // V3 Mint/Burn
            if topic0 == *decoders::V3_MINT_TOPIC || topic0 == decoders::V3_BURN_TOPIC {
                self.process_v3_mint_burn_log(log);
                continue;
            }
            // Curve TokenExchange
            if topic0 == *decoders::CURVE_TOKEN_EXCHANGE_TOPIC
                || topic0 == *decoders::CURVE_V2_TOKEN_EXCHANGE_TOPIC
            {
                self.process_curve_swap_log(log);
                continue;
            }
            // Balancer Swap
            if topic0 == *decoders::BALANCER_SWAP_TOPIC {
                self.process_balancer_swap_log(log);
                continue;
            }
        }
    }

    fn process_v2_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 128 {
            return;
        }
        let amt0_in = u128_from_be_bytes(&log.data[..32]);
        let amt1_in = u128_from_be_bytes(&log.data[32..64]);
        let amt0_out = u128_from_be_bytes(&log.data[64..96]);
        let amt1_out = u128_from_be_bytes(&log.data[96..128]);
        self.apply_v2_swap(&log.address, amt0_in, amt1_in, amt0_out, amt1_out);
    }

    fn process_v2_sync_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 64 {
            return;
        }
        let r0 = u128_from_be_bytes(&log.data[..32]);
        let r1 = u128_from_be_bytes(&log.data[32..64]);
        self.apply_v2_sync(&log.address, r0, r1);
    }

    fn process_v3_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_swap(log) {
            self.apply_v3_swap(
                &log.address,
                decoded.sqrt_price_x96,
                decoded.tick,
                decoded.liquidity,
            );
        }
    }

    fn process_v3_mint_burn_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_mint_burn(log) {
            self.apply_v3_mint_burn(
                &log.address,
                decoded.tick_lower,
                decoded.tick_upper,
                decoded.amount,
            );
        }
    }

    fn process_curve_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_curve_swap(log) {
            if let Some(PoolState::Curve(state)) = self.pools.get_mut(&log.address) {
                let coin_sold = decoded.coin_sold as usize;
                let coin_bought = decoded.coin_bought as usize;
                if coin_sold < state.balances.len() && coin_bought < state.balances.len() {
                    state.balances[coin_sold] =
                        state.balances[coin_sold].saturating_add(decoded.amount_sold);
                    state.balances[coin_bought] =
                        state.balances[coin_bought].saturating_sub(decoded.amount_bought);
                }
            }
        }
    }

    fn process_balancer_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_balancer_swap(log) {
            if let Some(PoolState::Balancer(state)) = self.pools.get_mut(&log.address) {
                let idx_in = state.token_index.get(&decoded.token_in);
                let idx_out = state.token_index.get(&decoded.token_out);
                if let (Some(&i_in), Some(&i_out)) = (idx_in, idx_out) {
                    if i_in < state.balances.len() && i_out < state.balances.len() {
                        state.balances[i_in] =
                            state.balances[i_in].saturating_add(decoded.amount_in);
                        state.balances[i_out] =
                            state.balances[i_out].saturating_sub(decoded.amount_out);
                    }
                }
            }
        }
    }
}

impl Default for PoolManager {
    fn default() -> Self {
        Self::new()
    }
}

fn u128_from_be_bytes(bytes: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    let start = bytes.len().saturating_sub(16);
    buf.copy_from_slice(&bytes[start..start + 16]);
    u128::from_be_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address, B256, U256};
    use crate::data::ExecutedLog;
    use crate::pool::decoders::V3_SWAP_TOPIC;

    fn wmatic() -> Address {
        address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270")
    }
    fn usdc() -> Address {
        address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
    }
    fn usdt() -> Address {
        address!("c2132d05d31c914a87c6611c10748aeb04b58e8f")
    }

    fn make_v2_pool(addr: Address, t0: Address, t1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                name: None,
                dex_type: DexType::UniswapV2,
                tick_spacing: None,
            },
            reserve0: r0,
            reserve1: r1,
        })
    }

    fn make_v3_pool(addr: Address, t0: Address, t1: Address, sqrt: U256, tick: i32, liq: u128) -> PoolState {
        PoolState::UniswapV3(UniswapV3PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing: Some(60),
            },
            sqrt_price_x96: sqrt,
            tick,
            liquidity: liq,
            ticks: HashMap::new(),
        })
    }

    fn encode_u256(value: u128) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let be = value.to_be_bytes();
        buf[16..32].copy_from_slice(&be);
        buf
    }

    fn encode_i32_right(value: i32) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let be = value.to_be_bytes();
        buf[28..32].copy_from_slice(&be);
        buf
    }

    // ---- PoolManager creation & management ----

    #[test]
    fn test_pm_new_and_empty() {
        let pm = PoolManager::new();
        assert_eq!(pm.pool_count(), 0);
        assert!(pm.pool_addresses().is_empty());
        assert!(pm.token_addresses().is_empty());
    }

    #[test]
    fn test_pm_add_and_get_pool() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        assert_eq!(pm.pool_count(), 1);
        let p = pm.get(&addr);
        assert!(p.is_some());
        assert_eq!(p.unwrap().address(), addr);
    }

    #[test]
    fn test_pm_get_mut_updates_reserves() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        if let Some(PoolState::UniswapV2(s)) = pm.get_mut(&addr) {
            s.reserve0 = 999;
        }
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 999);
        } else {
            panic!("expected V2 pool");
        }
    }

    #[test]
    fn test_pm_pool_addresses() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 1, 1));
        let addrs = pm.pool_addresses();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&a));
        assert!(addrs.contains(&b));
    }

    #[test]
    fn test_pm_token_addresses_and_pools_for_token() {
        let mut pm = PoolManager::new();
        let p1 = address!("1111111111111111111111111111111111111111");
        let p2 = address!("2222222222222222222222222222222222222222");
        pm.add_pool(make_v2_pool(p1, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(p2, usdt(), wmatic(), 1, 1));
        let tokens = pm.token_addresses();
        assert_eq!(tokens.len(), 3);
        assert!(tokens.contains(&wmatic()));
        let wmatic_pools = pm.pools_for_token(&wmatic()).unwrap();
        assert_eq!(wmatic_pools.len(), 2);
    }

    #[test]
    fn test_pm_find_pair_pool() {
        let mut pm = PoolManager::new();
        let p1 = address!("1111111111111111111111111111111111111111");
        pm.add_pool(make_v2_pool(p1, usdc(), wmatic(), 1, 1));
        assert_eq!(pm.find_pair_pool(&usdc(), &wmatic()), Some(p1));
        assert!(pm.find_pair_pool(&usdc(), &usdt()).is_none());
    }

    #[test]
    fn test_pm_arbitrage_pairs_caching() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 1, 1));
        let pairs = pm.arbitrage_pairs();
        assert_eq!(pairs.len(), 1);
        let pairs2 = pm.arbitrage_pairs();
        assert_eq!(pairs, pairs2);
        let c = address!("cccccccccccccccccccccccccccccccccccccccc");
        pm.add_pool(make_v2_pool(c, usdc(), usdt(), 1, 1));
        assert_eq!(pm.arbitrage_pairs().len(), 3);
    }

    // ---- V2 state updates ----

    #[test]
    fn test_pm_apply_v2_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.apply_v2_swap(&addr, 100, 0, 0, 50);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1100);
            assert_eq!(s.reserve1, 1950);
        }
    }

    #[test]
    fn test_pm_apply_v2_sync() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.apply_v2_sync(&addr, 999, 1999);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 999);
            assert_eq!(s.reserve1, 1999);
        }
    }

    #[test]
    fn test_pm_apply_v2_swap_unknown_address_noop() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let unknown = address!("ffffffffffffffffffffffffffffffffffffffff");
        pm.apply_v2_swap(&unknown, 100, 0, 0, 50);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    // ---- V3 state updates ----

    #[test]
    fn test_pm_apply_v3_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_swap(&addr, U256::from(2u128 << 96), 10, 999_000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.sqrt_price_x96, U256::from(2u128 << 96));
            assert_eq!(s.tick, 10);
            assert_eq!(s.liquidity, 999_000);
        }
    }

    #[test]
    fn test_pm_apply_v3_mint_burn_add() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_mint_burn(&addr, -100, 100, 500_000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.liquidity, 1_500_000);
            assert_eq!(*s.ticks.get(&-100).unwrap(), 500_000);
            assert_eq!(*s.ticks.get(&100).unwrap(), -500_000);
        }
    }

    #[test]
    fn test_pm_apply_v3_mint_burn_remove() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_mint_burn(&addr, -100, 100, -200_000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.liquidity, 800_000);
            assert_eq!(*s.ticks.get(&-100).unwrap(), -200_000);
            assert_eq!(*s.ticks.get(&100).unwrap(), 200_000);
        }
    }

    // ---- initialized_count ----

    #[test]
    fn test_pm_initialized_count() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let c = address!("cccccccccccccccccccccccccccccccccccccccc");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1000, 2000));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 0, 0));
        pm.add_pool(make_v3_pool(c, wmatic(), usdt(), U256::ZERO, 0, 0));
        assert_eq!(pm.initialized_count(), 1);
    }

    // ---- update_from_logs ----

    fn make_v2_swap_log(pool: Address, amt0_in: u128, amt1_in: u128, amt0_out: u128, amt1_out: u128) -> ExecutedLog {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&encode_u256(amt0_in));
        data.extend_from_slice(&encode_u256(amt1_in));
        data.extend_from_slice(&encode_u256(amt0_out));
        data.extend_from_slice(&encode_u256(amt1_out));
        ExecutedLog {
            address: pool,
            topics: vec![SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    fn make_v2_sync_log(pool: Address, r0: u128, r1: u128) -> ExecutedLog {
        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&encode_u256(r0));
        data.extend_from_slice(&encode_u256(r1));
        ExecutedLog {
            address: pool,
            topics: vec![SYNC_TOPIC],
            data: data.into(),
        }
    }

    fn make_v3_swap_log(pool: Address, sqrt: U256, liq: u128, tick: i32) -> ExecutedLog {
        let mut data = Vec::with_capacity(160);
        // amount0 and amount1 come before sqrt/liq/tick in the V3 Swap event data
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1
        let mut b = [0u8; 32];
        b.copy_from_slice(&sqrt.to_be_bytes::<32>());
        data.extend_from_slice(&b);
        data.extend_from_slice(&encode_u256(liq));
        data.extend_from_slice(&encode_i32_right(tick));
        ExecutedLog {
            address: pool,
            topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    #[test]
    fn test_pm_update_from_logs_v2_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = make_v2_swap_log(addr, 100, 0, 0, 50);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1100);
            assert_eq!(s.reserve1, 1950);
        }
    }

    #[test]
    fn test_pm_update_from_logs_v2_sync() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = make_v2_sync_log(addr, 555, 666);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 555);
            assert_eq!(s.reserve1, 666);
        }
    }

    #[test]
    fn test_pm_update_from_logs_v3_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        let new_sqrt = U256::from(15u128 << 96) / U256::from(10);
        let log = make_v3_swap_log(addr, new_sqrt, 999_000, 5);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.sqrt_price_x96, new_sqrt);
            assert_eq!(s.liquidity, 999_000);
            assert_eq!(s.tick, 5);
        }
    }

    #[test]
    fn test_pm_update_from_logs_ignores_untracked_pool() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let unknown = address!("ffffffffffffffffffffffffffffffffffffffff");
        pm.update_from_logs(&[make_v2_swap_log(unknown, 100, 0, 0, 50)]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    #[test]
    fn test_pm_update_from_logs_empty_topics_skipped() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = ExecutedLog {
            address: addr,
            topics: vec![],
            data: Default::default(),
        };
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    // ---- PoolState helpers ----

    #[test]
    fn test_pool_info_is_concentrated_liquidity() {
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let v2 = make_v2_pool(a, usdc(), wmatic(), 1, 1);
        assert!(!v2.info().is_concentrated_liquidity());
        let v3_addr = address!("3333333333333333333333333333333333333333");
        let v3 = make_v3_pool(v3_addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1);
        assert!(v3.info().is_concentrated_liquidity());
    }

    // ---- PoolState::info / info_mut ----

    #[test]
    fn test_pool_state_info_mut() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.get_mut(&addr).unwrap().info_mut().fee = 100;
        assert_eq!(pm.get(&addr).unwrap().info().fee, 100);
    }

    // ---- u128_from_be_bytes helper ----

    #[test]
    fn test_u128_from_be_bytes_basic() {
        let mut buf = [0u8; 32];
        buf[16..32].copy_from_slice(&1000u128.to_be_bytes());
        assert_eq!(super::u128_from_be_bytes(&buf), 1000);
    }

    #[test]
    fn test_u128_from_be_bytes_zero() {
        let buf = [0u8; 32];
        assert_eq!(super::u128_from_be_bytes(&buf), 0);
    }

    // ---- decode_v2_reserves_from_storage ----

    fn make_v2_storage_raw(reserve0: u128, reserve1: u128, block_ts: u32) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&block_ts.to_be_bytes());
        let r1_be = reserve1.to_be_bytes();
        bytes[4..18].copy_from_slice(&r1_be[2..]);
        let r0_be = reserve0.to_be_bytes();
        bytes[18..32].copy_from_slice(&r0_be[2..]);
        U256::from_be_bytes(bytes)
    }

    #[test]
    fn test_decode_v2_small_reserves() {
        let raw = make_v2_storage_raw(1, 2, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, 1);
        assert_eq!(r1, 2);
    }

    #[test]
    fn test_decode_v2_max_reserves() {
        let max = (1u128 << 112) - 1;
        let raw = make_v2_storage_raw(max, max, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, max);
        assert_eq!(r1, max);
    }

    #[test]
    fn test_decode_v2_zero_reserves() {
        let raw = make_v2_storage_raw(0, 0, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, 0);
        assert_eq!(r1, 0);
    }

    // ---- decode_v3_state_from_storage ----

    fn make_v3_slot0_raw(sqrt_price_x96: U256, tick: i32) -> U256 {
        let mut bytes = [0u8; 32];
        let sqrt_be = sqrt_price_x96.to_be_bytes::<32>();
        bytes[12..32].copy_from_slice(&sqrt_be[12..32]);
        let tick_u24 = (tick as u32) & 0xFFFFFF;
        let tick_be = tick_u24.to_be_bytes();
        bytes[9] = tick_be[1];
        bytes[10] = tick_be[2];
        bytes[11] = tick_be[3];
        U256::from_be_bytes(bytes)
    }

    #[test]
    fn test_decode_v3_minimal() {
        let slot0 = make_v3_slot0_raw(U256::from(1u128), 0);
        let slot1 = U256::ZERO;
        let (sqrt, tick, liq) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(sqrt, U256::from(1u128));
        assert_eq!(tick, 0);
        assert_eq!(liq, 0);
    }

    #[test]
    fn test_decode_v3_negative_tick() {
        let slot0 = make_v3_slot0_raw(U256::from(1u128), -10);
        let slot1 = U256::ZERO;
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(tick, -10);
    }

    #[test]
    fn test_decode_v3_typical() {
        let sqrt = U256::from(1234567890u128) << 64;
        let slot0 = make_v3_slot0_raw(sqrt, 50000);
        let slot1 = U256::from(1_000_000_000u128);
        let (sqrt_out, tick_out, liq_out) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(sqrt_out, sqrt);
        assert_eq!(tick_out, 50000);
        assert_eq!(liq_out, 1_000_000_000);
    }

    #[test]
    fn test_decode_v3_tick_min_int24() {
        // int24 minimum: -8,388,608 (0x800000 as 24-bit two's complement)
        let slot0 = make_v3_slot0_raw(U256::from(1u128), -8388608);
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, U256::ZERO);
        assert_eq!(tick, -8388608);
    }

    #[test]
    fn test_decode_v3_tick_max_int24() {
        // int24 maximum: 8,388,607 (0x7FFFFF)
        let slot0 = make_v3_slot0_raw(U256::from(1u128), 8388607);
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, U256::ZERO);
        assert_eq!(tick, 8388607);
    }
}
