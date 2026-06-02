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
    #[serde(rename = "type")]
    pub pool_type: String,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub name: Option<String>,
    #[serde(default)]
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
        let cap = pool_addrs.len().min(20).max(1);
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
        let data = Bytes::copy_from_slice(&GET_RESERVES_SELECTOR);
        let result = rpc.call(pool, data, block).await.ok()?;
        if result.len() < 64 {
            return None;
        }
        // ABI decode: 3 × uint256 (reserve0, reserve1, blockTimestampLast), but reserve0/1 are uint112
        // Alloy returns 0-padded bytes; read the last 32 bytes for each value
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&result[..32]);
        let r0 = U256::from_be_bytes(buf).as_limbs()[0] as u128;
        buf.copy_from_slice(&result[32..64]);
        let r1 = U256::from_be_bytes(buf).as_limbs()[0] as u128;
        Some((r0, r1))
    }

    /// Fetch V3 pool slot0() + liquidity() at a historical block.
    async fn fetch_v3_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<(U256, i32, u128)> {
        // --- slot0() ---
        let result = rpc.call(pool, V3_SLOT0_SELECTOR.clone(), block).await.ok()?;
        if result.len() < 96 {
            return None;
        }
        let mut buf = [0u8; 32];
        // sqrtPriceX96 (uint160 → 32 bytes)
        buf.copy_from_slice(&result[..32]);
        let sqrt_price_x96 = U256::from_be_bytes(buf);
        // tick (int24 → int256 → 32 bytes, last 4 bytes are the int24 as i32)
        let mut tick_bytes = [0u8; 4];
        tick_bytes.copy_from_slice(&result[60..64]);
        let tick = i32::from_be_bytes(tick_bytes);

        // --- liquidity() ---
        let result = rpc.call(pool, V3_LIQUIDITY_SELECTOR.clone(), block).await.ok()?;
        if result.len() < 32 {
            return None;
        }
        buf.copy_from_slice(&result[..32]);
        // uint128 is left-padded to 32 bytes; value is in least significant 128 bits
        let liquidity = U256::from_be_bytes(buf).as_limbs()[0] as u128;

        Some((sqrt_price_x96, tick, liquidity))
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
