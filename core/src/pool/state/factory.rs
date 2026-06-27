use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use alloy::primitives::{keccak256, Address, Bytes, U256};
use futures::future::join_all;
use tokio::sync::Semaphore;

use crate::pool::dex_type::DexType;
use crate::rpc::RpcClient;
use crate::pool::state::manager::PoolManager;
use crate::pool::state::pool_types::{PoolInfo, PoolState, UniswapV2PoolState, UniswapV3PoolState, CurvePoolState, CurvePoolVariant, BalancerPoolState, BalancerPoolVariant};
pub enum PoolInitResult {
    V2Reserves(u128, u128),
    V3State(U256, i32, u128, std::collections::BTreeMap<i32, i128>),
    /// (tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index)
    BalancerState(Vec<Address>, Vec<u128>, Vec<u128>, u32, BalancerPoolVariant, Option<u128>, Vec<u128>, Option<usize>),
    /// (tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool)
    CurveState(Vec<Address>, Vec<u128>, u128, u32, CurvePoolVariant, Option<u128>, Vec<u128>, Option<Address>),
}

/// getReserves() selector
const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// balances(int128) selector for Curve pools
const CURVE_BALANCES_SELECTOR: [u8; 4] = [0x49, 0x7b, 0x66, 0x78];

/// A() selector for Curve pools amplification coefficient
const CURVE_A_SELECTOR: [u8; 4] = [0x0f, 0x0b, 0x7c, 0x7e];

/// fee() selector for Curve pools �?� swap fee (parts per 10??�??)
const CURVE_FEE_SELECTOR: [u8; 4] = [0xdd, 0xca, 0x3f, 0x43];

/// get_A() selector for Curve CryptoSwap V2 pools
const CURVE_GET_A_SELECTOR: [u8; 4] = [0x4d, 0x30, 0xa4, 0x7f];

/// gamma() selector for Curve CryptoSwap V2 pools
const CURVE_GAMMA_SELECTOR: [u8; 4] = [0x67, 0x1d, 0x47, 0x23];

/// price_scale() selector for Curve CryptoSwap V2 pools (returns all scales as array)
const CURVE_PRICE_SCALE_SELECTOR: [u8; 4] = [0x5e, 0x0d, 0x7a, 0x5a];

/// price_oracle(uint256) selector for Curve CryptoSwap V2 pools
#[allow(dead_code)]
const CURVE_PRICE_ORACLE_SELECTOR: [u8; 4] = [0xaa, 0x1e, 0x29, 0x84];

/// base_pool() selector for Curve Metapools
const CURVE_BASE_POOL_SELECTOR: [u8; 4] = [0x9c, 0xec, 0x6e, 0xae];

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
/// getPoolTokens(bytes32) selector for Balancer V2 vault
static GET_POOL_TOKENS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getPoolTokens(bytes32)");
    Bytes::copy_from_slice(&hash[..4])
});

/// getNormalizedWeights() selector for Balancer weighted pools
static GET_NORMALIZED_WEIGHTS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getNormalizedWeights()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getSwapFeePercentage() selector for Balancer pools
static GET_SWAP_FEE_PERCENTAGE_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getSwapFeePercentage()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getAmplificationParameter() selector for Balancer stable pools
static GET_AMPLIFICATION_PARAMETER_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getAmplificationParameter()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getScalingFactors() selector for Balancer composable/boosted pools
static GET_SCALING_FACTORS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getScalingFactors()");
    Bytes::copy_from_slice(&hash[..4])
});

/// tickBitmap(int16) selector for Uniswap V3 tick bitmap queries
static V3_TICK_BITMAP_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"tickBitmap(int16)");
    Bytes::copy_from_slice(&hash[..4])
});

/// ticks(int24) selector for Uniswap V3 per-tick data queries
static V3_TICKS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"ticks(int24)");
    Bytes::copy_from_slice(&hash[..4])
});

impl PoolManager {
    pub async fn init_from_rpc(&mut self, rpc: &RpcClient, block_num: u64) {
        let pool_addrs: Vec<Address> = self.pools.keys().copied().collect();
        let max_concurrent = self.concurrency_limit as usize;
        let cap = pool_addrs.len().clamp(1, max_concurrent);
        let semaphore = Arc::new(Semaphore::new(cap));

        // Snapshot pool metadata before spawning tasks
        let vault = self.balancer_vault;
        let pool_meta: Vec<(DexType, Option<[u8; 32]>, i32, Option<Address>)> = pool_addrs
            .iter()
            .map(|addr| match self.pools.get(addr) {
                Some(PoolState::UniswapV2(s)) => (DexType::UniswapV2, None, 0, s.info.factory),
                Some(PoolState::UniswapV3(state)) => (
                    DexType::UniswapV3,
                    None,
                    state.info.tick_spacing.unwrap_or(60) as i32,
                    None,
                ),
                Some(PoolState::Curve(_)) => (DexType::Curve, None, 0, None),
                Some(PoolState::Balancer(_)) => (DexType::Balancer, None, 0, None),
                None => (DexType::UniswapV2, None, 0, None),
            })
            .collect();

        let tasks: Vec<_> = pool_addrs
            .iter()
            .zip(pool_meta.iter())
            .map(|(addr, (dt, pool_id, tick_spacing, factory))| {
                let rpc = rpc.clone();
                let sem = Arc::clone(&semaphore);
                let addr = *addr;
                let dt = *dt;
                let pool_id = *pool_id;
                let tick_spacing = *tick_spacing;
                let factory = *factory;
                async move {
                    let _permit = sem.acquire_owned().await.ok();
                    Self::fetch_pool_state(&rpc, addr, dt, pool_id, tick_spacing, vault, factory, block_num).await
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
                Some(PoolInitResult::V3State(sqrt, tick, liq, initialized_ticks)) => {
                    if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(addr) {
                        state.sqrt_price_x96 = sqrt;
                        state.tick = tick;
                        state.liquidity = liq;
                        state.ticks = initialized_ticks;
                        if state.ticks.is_empty() {
                            tracing::warn!("V3 pool {} initialized with empty tick map", addr);
                        } else {
                            tracing::debug!("V3 pool {} initialized with {} ticks", addr, state.ticks.len());
                        }
                        if sqrt.is_zero() {
                            tracing::warn!("V3 pool {} initialized with zero sqrt price", addr);
                        }
                    }
                }
                Some(PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index)) => {
                    if let Some(PoolState::Balancer(state)) = self.pools.get_mut(addr) {
                        state.balances = balances;
                        state.weights = weights;
                        state.info.fee = fee_bps;
                        state.pool_variant = variant;
                        state.amplification = amplification;
                        state.scaling_factors = scaling_factors;
                        state.bpt_index = bpt_index;
                        if tokens.len() >= 2 {
                            state.info.token0 = tokens[0];
                            state.info.token1 = tokens[1];
                        }
                        state.token_index.clear();
                        for (i, token) in tokens.iter().enumerate() {
                            state.token_index.insert(*token, i);
                        }
                        for token in &tokens {
                            if !token.is_zero() {
                                self.token_index.entry(*token).or_default().push(*addr);
                            }
                        }
                    }
                }
                Some(PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool)) => {
                    if let Some(PoolState::Curve(state)) = self.pools.get_mut(addr) {
                        state.balances = balances;
                        state.a_coeff = a_coeff;
                        state.info.fee = fee_bps;
                        state.pool_variant = variant;
                        state.gamma = gamma;
                        state.price_scale = price_scale;
                        state.base_pool = base_pool;
                        state.token_index.clear();
                        for (i, token) in tokens.iter().enumerate() {
                            state.token_index.insert(*token, i);
                            if i == 0 {
                                state.info.token0 = *token;
                            } else if i == 1 {
                                state.info.token1 = *token;
                            }
                        }
                        for token in &tokens {
                            if !token.is_zero() {
                                self.token_index.entry(*token).or_default().push(*addr);
                            }
                        }
                    }
                }
                None => {
                    tracing::warn!("Failed to fetch state for pool {}", addr);
                }
            }
        }
    }

    /// Filter pools to only those that have non-empty bytecode at the target block.
    /// Uses concurrent `eth_getCode` calls bounded by the concurrency limit.
    /// Pools with empty code (not yet deployed or self-destructed) are excluded.
    pub async fn filter_existing_pools(
        rpc: &RpcClient,
        pools: &[PoolInfo],
        block_num: u64,
        concurrency_limit: usize,
    ) -> Vec<PoolInfo> {
        if pools.is_empty() {
            return Vec::new();
        }
        let cap = pools.len().clamp(1, concurrency_limit);
        let semaphore = Arc::new(Semaphore::new(cap));

        let tasks: Vec<_> = pools
            .iter()
            .map(|info| {
                let rpc = rpc.clone();
                let sem = Arc::clone(&semaphore);
                let addr = info.address;
                let info = info.clone();
                async move {
                    let _permit = sem.acquire_owned().await.ok();
                    match rpc.get_code(addr, block_num).await {
                        Ok(code) if !code.is_empty() => Some(info),
                        _ => None,
                    }
                }
            })
            .collect();

        let results = join_all(tasks).await;
        results.into_iter().flatten().collect()
    }

    /// Fetch the appropriate on-chain state for a pool based on its type.
    async fn fetch_pool_state(
        rpc: &RpcClient,
        pool: Address,
        dt: DexType,
        pool_id: Option<[u8; 32]>,
        tick_spacing: i32,
        vault: Option<Address>,
        factory: Option<Address>,
        block: u64,
    ) -> Option<PoolInitResult> {
        match dt {
            DexType::UniswapV2 => {
                let (r0, r1) = Self::fetch_v2_reserves(rpc, pool, block, factory).await?;
                Some(PoolInitResult::V2Reserves(r0, r1))
            }
            DexType::UniswapV3 => {
                let (sqrt, tick, liq, ticks) = Self::fetch_v3_state(rpc, pool, block, tick_spacing).await?;
                Some(PoolInitResult::V3State(sqrt, tick, liq, ticks))
            }
            DexType::Balancer => {
                let vault = vault?;
                let pool_id = pool_id?;
                Self::fetch_balancer_state(rpc, vault, pool, &pool_id, block).await.ok()
            }
            DexType::Curve => {
                Self::fetch_curve_state(rpc, pool, block).await
            }
        }
    }

    async fn call_once(rpc: &RpcClient, pool: Address, data: Bytes, block: u64) -> Result<Bytes, ()> {
        rpc.call(pool, data, block).await.map_err(|_| ())
    }

    async fn fetch_v2_reserves(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        factory: Option<Address>,
    ) -> Option<(u128, u128)> {
        let slots: Vec<U256> = crate::types::v2_storage_slots_for_factory(factory)
            .iter()
            .map(|&s| U256::from(s))
            .collect();

        // M9: eth_getStorageAt is primary path (cheaper, works on more nodes).
        // eth_call getReserves() is the fallback.
        if let Some(reserves) = Self::fetch_v2_reserves_storage(rpc, pool, block, &slots).await {
            if Self::validate_v2_reserves(reserves) {
                return Some(reserves);
            }
            tracing::trace!("storage reserves failed validation ({},{}), trying eth_call", reserves.0, reserves.1);
        }

        // Fallback: eth_call getReserves()
        let data = Bytes::copy_from_slice(&GET_RESERVES_SELECTOR);
        if let Ok(result) = Self::call_once(rpc, pool, data, block).await {
            if result.len() >= 64 {
                let r0 = Self::decode_u128_from_abi_word(&result[..32]);
                let r1 = Self::decode_u128_from_abi_word(&result[32..64]);
                let reserves = (r0, r1);
                if Self::validate_v2_reserves(reserves) {
                    return Some(reserves);
                }
                tracing::trace!("eth_call reserves failed validation ({},{}), returning raw", r0, r1);
                return Some(reserves);
            }
        }
        tracing::trace!("eth_call getReserves() failed for {}", pool);
        None
    }

    /// Validate V2 reserves: both > 0 and ratio is within 100x (M9).
    fn validate_v2_reserves(reserves: (u128, u128)) -> bool {
        let (r0, r1) = reserves;
        if r0 == 0 || r1 == 0 {
            return false;
        }
        // Reject extreme ratios (>100:1) which indicate corrupted data
        let (big, small) = if r0 > r1 { (r0, r1) } else { (r1, r0) };
        if small == 0 { return false; }
        let ratio = big / small;
        ratio < 100
    }

    /// Given raw slot 6 (packed uint112 reserve0 | uint112 reserve1 | uint32 blockTimestampLast),
    /// decode reserve0 and reserve1.
    fn decode_v2_reserves_from_storage(raw: U256) -> (u128, u128) {
        let mask: U256 = (U256::from(1u128) << 112) - U256::from(1u128);
        let masked_r0 = raw & mask;
        let masked_r1 = (raw >> U256::from(112u64)) & mask;
        let lo_r0 = masked_r0.as_limbs();
        let lo_r1 = masked_r1.as_limbs();
        let r0 = (lo_r0[0] as u128) | ((lo_r0[1] as u128) << 64);
        let r1 = (lo_r1[0] as u128) | ((lo_r1[1] as u128) << 64);
        (r0, r1)
    }

    /// Decode a u128 from a 32-byte ABI-encoded word (right-aligned uint128/uint112).
    /// Handles values up to 2^128-1 without truncation.
    fn decode_u128_from_abi_word(word: &[u8]) -> u128 {
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&word[16..32]);
        u128::from_be_bytes(buf)
    }

    /// Fallback: fetch V2 reserves via eth_getStorageAt.
    /// Tries the given storage slots and returns the first that decodes to non-zero.
    /// If all slots return zero, falls back to the first slot as last resort.
    async fn fetch_v2_reserves_storage(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        slots: &[U256],
    ) -> Option<(u128, u128)> {
        for &slot in slots {
            if let Ok(raw) = rpc.get_storage_at(pool, slot, block).await {
                let (r0, r1) = Self::decode_v2_reserves_from_storage(raw);
                if r0 > 0 || r1 > 0 {
                    return Some((r0, r1));
                }
            }
        }
        // Last resort: try the first slot even if zero
        let first = slots.first().copied().unwrap_or(U256::from(6u64));
        let raw = rpc.get_storage_at(pool, first, block).await.ok()?;
        Some(Self::decode_v2_reserves_from_storage(raw))
    }

    /// Fetch V3 pool slot0() + liquidity() + tick data at a historical block.
    /// Also bootstraps the tick map from on-chain via `tickBitmap` and `ticks` calls,
    /// fixing C2: pre-existing LP positions are now visible from the first block.
    async fn fetch_v3_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        tick_spacing: i32,
    ) -> Option<(U256, i32, u128, std::collections::BTreeMap<i32, i128>)> {
        let slot0_result = Self::call_once(rpc, pool, V3_SLOT0_SELECTOR.clone(), block).await;
        let liq_result = Self::call_once(rpc, pool, V3_LIQUIDITY_SELECTOR.clone(), block).await;
        if let (Ok(slot0), Ok(liq)) = (slot0_result.as_ref(), liq_result.as_ref()) {
            if slot0.len() >= 96 && liq.len() >= 32 {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(&slot0[..32]);
                let sqrt_price_x96 = U256::from_be_bytes(buf);
                let mut tick_bytes = [0u8; 4];
                tick_bytes.copy_from_slice(&slot0[60..64]);
                let tick = i32::from_be_bytes(tick_bytes);
                let liquidity = Self::decode_u128_from_abi_word(&liq[..32]);

                // Bootstrap tick data from on-chain tick bitmap + per-tick queries
                // This makes pre-existing LP positions visible from the first block.
                let initialized_ticks = Self::fetch_v3_initialized_ticks(rpc, pool, tick, tick_spacing, block).await;

                return Some((sqrt_price_x96, tick, liquidity, initialized_ticks));
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
        let (sqrt, tick, liq) = Self::fetch_v3_state_storage(rpc, pool, block).await?;
        let initialized_ticks = Self::fetch_v3_initialized_ticks(rpc, pool, tick, tick_spacing, block).await;
        Some((sqrt, tick, liq, initialized_ticks))
    }

    /// Fetch initialized tick liquidity nets from a V3 pool contract.
    ///
    /// Queries the tick bitmap for 5 word positions centered around the current
    /// tick (~1280 tick range), then fetches `liquidityNet` for each initialized
    /// tick found (up to 200 ticks). This bootstraps pre-existing LP positions
    /// so that V3 quoting in the first block is accurate (fixes C2).
    async fn fetch_v3_initialized_ticks(
        rpc: &RpcClient,
        pool: Address,
        current_tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> std::collections::BTreeMap<i32, i128> {
        let max_ticks = 200usize;
        let mut ticks = std::collections::BTreeMap::new();

        // Compressed tick: floor division that handles negatives
        let compressed = if current_tick < 0 && current_tick % tick_spacing != 0 {
            current_tick / tick_spacing - 1
        } else {
            current_tick / tick_spacing
        };
        let center_word = compressed >> 8;

        // Scan 5 word positions (���1280 tick range) centered on current tick
        for word_offset in -2i16..=2i16 {
            if ticks.len() >= max_ticks {
                break;
            }
            let w = center_word.wrapping_add(word_offset as i32);

            // Build calldata for tickBitmap(int16)
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&V3_TICK_BITMAP_SELECTOR);
            let mut arg = [0u8; 32];
            let w_i16 = w as i16;
            let w_be = w_i16.to_be_bytes();
            arg[30..32].copy_from_slice(&w_be);
            if w_i16 < 0 {
                arg[..30].fill(0xFF);
            }
            calldata.extend_from_slice(&arg);

            let bitmap_bytes = match Self::call_once(rpc, pool, Bytes::from(calldata), block).await {
                Ok(b) if b.len() >= 32 => b,
                _ => continue,
            };

            let bitmap = U256::from_be_slice(&bitmap_bytes[..32]);
            if bitmap.is_zero() {
                continue;
            }

            // Iterate over set bits in the 256-bit bitmap
            let mut bits_remaining = bitmap;
            for bit_pos in 0u8..=255u8 {
                if ticks.len() >= max_ticks {
                    break;
                }
                if !bits_remaining.bit(bit_pos as usize) {
                    continue;
                }
                bits_remaining = bits_remaining & !(U256::from(1u128) << bit_pos as usize);

                // Decode tick index: compressed = w * 256 + bit_pos
                let compressed_tick = (w << 8) | (bit_pos as i32);
                // For negative word positions, ensure proper sign extension
                let compressed_tick = if w < 0 && bit_pos == 0 {
                    compressed_tick
                } else {
                    compressed_tick
                };
                let actual_tick = compressed_tick.wrapping_mul(tick_spacing);

                // Fetch liquidityNet for this tick via ticks(int24)
                if let Some(liq_net) = Self::fetch_v3_tick_liquidity_net(rpc, pool, actual_tick, block).await {
                    if liq_net != 0 {
                        ticks.insert(actual_tick, liq_net);
                    }
                }
            }
        }

        if !ticks.is_empty() {
            tracing::debug!(
                "Bootstrapped {} initialized ticks for V3 pool {} at block {}",
                ticks.len(),
                pool,
                block,
            );
        }

        ticks
    }

    /// Fetch liquidityNet for a single tick from a V3 pool via `ticks(int24)`.
    async fn fetch_v3_tick_liquidity_net(
        rpc: &RpcClient,
        pool: Address,
        tick: i32,
        block: u64,
    ) -> Option<i128> {
        // Build calldata for ticks(int24)
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&V3_TICKS_SELECTOR);
        let mut arg = [0u8; 32];
        let tick_bytes = tick.to_be_bytes();
        arg[29..32].copy_from_slice(&tick_bytes[1..4]);
        if tick < 0 {
            arg[..29].fill(0xFF);
        }
        calldata.extend_from_slice(&arg);

        let result = Self::call_once(rpc, pool, Bytes::from(calldata), block).await.ok()?;

        // ABI decode: (uint128 liquidityGross, int128 liquidityNet, ...)
        // Tuple with ABIEncoderV2: 8 fields ?� 32 bytes = 256 bytes
        if result.len() < 64 {
            return None;
        }

        // Check initialized flag (8th element, last 32-byte slot)
        let init_byte = result[result.len() - 1];
        if init_byte == 0 {
            return None;
        }

        // Decode int128 liquidityNet from bytes 48..64 (second 32-byte slot, lower 16 bytes)
        let mut net_bytes = [0u8; 16];
        net_bytes.copy_from_slice(&result[48..64]);
        let liq_net = i128::from_be_bytes(net_bytes);

        Some(liq_net)
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

    /// Decode an int128 from raw storage bytes (big-endian, right-aligned).
    #[allow(dead_code)]
    fn decode_i128_from_be_bytes(bytes: &[u8]) -> i128 {
        let mut buf = [0u8; 16];
        let start = bytes.len().saturating_sub(16);
        let copy_len = bytes.len().min(16);
        buf[16 - copy_len..].copy_from_slice(&bytes[start..start + copy_len]);
        i128::from_be_bytes(buf)
    }

    /// Process a list of executed logs from a single transaction, updating pool state
    /// for any Swap or Sync events emitted by tracked pools.
    async fn fetch_balancer_state(
        rpc: &RpcClient,
        vault: Address,
        pool: Address,
        pool_id: &[u8; 32],
        block: u64,
    ) -> anyhow::Result<PoolInitResult> {
        // --- Step 1: getPoolTokens from vault ---
        let data = {
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&GET_POOL_TOKENS_SELECTOR);
            calldata.extend_from_slice(pool_id);
            Bytes::from(calldata)
        };

        let result = rpc.call(vault, data, block).await?;
        let return_data = result.0;
        if return_data.len() < 96 {
            anyhow::bail!("Balancer getPoolTokens returned too short data");
        }

        // ABI decode: (address[], uint256[], uint256)
        let tokens_offset = U256::from_be_slice(&return_data[..32]);
        let balances_offset = U256::from_be_slice(&return_data[32..64]);
        let tokens_len_offset = 32 + tokens_offset.as_limbs()[0] as usize;
        let token_count = U256::from_be_slice(&return_data[tokens_len_offset..tokens_len_offset + 32]);
        let token_count = token_count.as_limbs()[0] as usize;

        let tokens_start = tokens_len_offset + 32;
        let mut tokens = Vec::with_capacity(token_count);
        for i in 0..token_count {
            let off = tokens_start + i * 32;
            let addr = Address::from_slice(&return_data[off + 12..off + 32]);
            tokens.push(addr);
        }

        let balances_start = 64 + balances_offset.as_limbs()[0] as usize + 32;
        let mut balances = Vec::with_capacity(token_count);
        for i in 0..token_count {
            let off = balances_start + i * 32;
            let bal = U256::from_be_slice(&return_data[off..off + 32]);
            balances.push(bal.as_limbs()[0] as u128);
        }

        if balances.len() < 2 {
            anyhow::bail!("Balancer pool has fewer than 2 tokens");
        }

        // --- Step 2: Fetch normalized weights from pool ---
        let weights = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_NORMALIZED_WEIGHTS_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let w_off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                    let w_count = if w_off + 32 <= result.0.len() {
                        U256::from_be_slice(&result.0[w_off..w_off + 32]).as_limbs()[0] as usize
                    } else {
                        0
                    };
                    let w_start = w_off + 32;
                    let mut w = Vec::with_capacity(w_count);
                    for j in 0..w_count {
                        let off = w_start + j * 32;
                        if off + 32 <= result.0.len() {
                            w.push(U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as u128);
                        }
                    }
                    w
                }
                _ => vec![],
            }
        };

        // --- Step 3: Fetch swap fee percentage from pool ---
        let fee_bps = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_SWAP_FEE_PERCENTAGE_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let chain_fee = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    // Balancer returns fee in 1e18 scale; convert to PoolInfo bps (1e6 = 100%)
                    (chain_fee / 1_000_000_000_000) as u32
                }
                _ => 0,
            }
        };

        // --- Step 4: Fetch scaling factors from pool ---
        let scaling_factors = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_SCALING_FACTORS_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 64 => {
                    let off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                    let count = if off + 32 <= result.0.len() {
                        U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as usize
                    } else { 0 };
                    let start = off + 32;
                    let mut sf = Vec::with_capacity(count);
                    for j in 0..count {
                        let pos = start + j * 32;
                        if pos + 32 <= result.0.len() {
                            sf.push(U256::from_be_slice(&result.0[pos..pos + 32]).as_limbs()[0] as u128);
                        }
                    }
                    sf
                }
                _ => vec![],
            }
        };

        // --- Step 5: Determine pool variant, amplification, and BPT index ---
        let (variant, amplification, bpt_index) = if !weights.is_empty() {
            // Has normalized weights �?� Weighted pool
            (BalancerPoolVariant::Weighted, None, None)
        } else {
            // No weights �?� try amplification parameter to detect Stable pool
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_AMPLIFICATION_PARAMETER_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 96 => {
                    let value = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    let precision = U256::from_be_slice(&result.0[64..96]).as_limbs()[0] as u128;
                    let effective_a = if precision > 0 { value / precision } else { value };

                    // Detect ComposableStable: has scaling factors and one token matches pool address
                    let bpt_idx = if scaling_factors.len() == token_count {
                        tokens.iter().position(|t| *t == pool)
                    } else {
                        None
                    };

                    let variant = if bpt_idx.is_some() {
                        BalancerPoolVariant::ComposableStable
                    } else {
                        BalancerPoolVariant::Stable
                    };

                    (variant, Some(effective_a), bpt_idx)
                }
                _ => (BalancerPoolVariant::Other, None, None),
            }
        };

        Ok(PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index))
    }

    /// Re-fetch a pool's on-chain state at a given block number (M3 fact-check support).
    ///
    /// Returns a fresh `PoolState` with data read directly from the chain via `eth_call`,
    /// bypassing the in-memory state. This is used by the EVM-based fact-check to verify
    /// opportunities against the actual on-chain state rather than the potentially-diverged
    /// cached state.
    pub async fn refetch_pool_state(
        &self,
        rpc: &RpcClient,
        addr: &Address,
        block: u64,
    ) -> Option<PoolState> {
        let pool = self.pools.get(addr)?;
        match pool {
            PoolState::UniswapV2(v2) => {
                let (r0, r1) = Self::fetch_v2_reserves(rpc, *addr, block, v2.info.factory).await?;
                Some(PoolState::UniswapV2(UniswapV2PoolState {
                    info: v2.info.clone(),
                    reserve0: r0,
                    reserve1: r1,
                }))
            }
            PoolState::UniswapV3(v3) => {
                let spacing = v3.info.tick_spacing.unwrap_or(60) as i32;
                let (sqrt, tick, liq, ticks) =
                    Self::fetch_v3_state(rpc, *addr, block, spacing).await?;
                Some(PoolState::UniswapV3(UniswapV3PoolState {
                    info: v3.info.clone(),
                    sqrt_price_x96: sqrt,
                    tick,
                    liquidity: liq,
                    ticks,
                    fee_growth_global_0_x128: U256::ZERO,
                    fee_growth_global_1_x128: U256::ZERO,
                }))
            }
            PoolState::Curve(curve) => {
                let result = Self::fetch_curve_state(rpc, *addr, block).await?;
                match result {
                    PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool) => {
                        let token_index: HashMap<Address, usize> = tokens
                            .iter()
                            .enumerate()
                            .map(|(i, t)| (*t, i))
                            .collect();
                        let mut info = curve.info.clone();
                        info.fee = fee_bps;
                        Some(PoolState::Curve(CurvePoolState {
                            info,
                            balances,
                            token_index,
                            a_coeff,
                            pool_variant: variant,
                            gamma,
                            price_scale,
                            base_pool,
                        }))
                    }
                    _ => None,
                }
            }
            PoolState::Balancer(bal) => {
                let vault = self.balancer_vault?;
                let pool_id = bal.pool_id?;
                let result = Self::fetch_balancer_state(rpc, vault, *addr, &pool_id, block).await.ok()?;
                match result {
                    PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index) => {
                        let token_index: HashMap<Address, usize> = tokens
                            .iter()
                            .enumerate()
                            .map(|(i, t)| (*t, i))
                            .collect();
                        let mut info = bal.info.clone();
                        info.fee = fee_bps;
                        Some(PoolState::Balancer(BalancerPoolState {
                            info,
                            balances,
                            token_index,
                            pool_id: Some(pool_id),
                            weights,
                            pool_variant: variant,
                            amplification,
                            scaling_factors,
                            bpt_index,
                        }))
                    }
                    _ => None,
                }
            }
        }
    }

    /// Fetch Curve pool state by calling `coins(int128)` and `balances(int128)` for each token index.
    /// Tries up to 16 token indices, stopping when a call returns zero address or fails.
    /// Detects pool variant (Plain/Meta/Crypto) and fetches variant-specific state.
    async fn fetch_curve_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<PoolInitResult> {
        static CURVE_COINS_SELECTOR: [u8; 4] = [0xc6, 0x61, 0x1f, 0x94]; // coins(int128)
        static CURVE_COINS_U256_SELECTOR: [u8; 4] = [0x19, 0x6c, 0xac, 0x5f]; // coins(uint256) �?� used by some forks
        let mut tokens = Vec::new();
        let mut balances = Vec::new();
        let max_tokens = 16u8;

        for i in 0u8..max_tokens {
            let token = {
                let mut calldata = Vec::with_capacity(36);
                calldata.extend_from_slice(&CURVE_COINS_SELECTOR);
                let mut arg = [0u8; 32];
                arg[31] = i;
                calldata.extend_from_slice(&arg);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        let addr = Address::from_slice(&result.0[12..32]);
                        if addr.is_zero() { break; }
                        addr
                    }
                    _ => {
                        let mut calldata2 = Vec::with_capacity(36);
                        calldata2.extend_from_slice(&CURVE_COINS_U256_SELECTOR);
                        let mut arg2 = [0u8; 32];
                        arg2[31] = i;
                        calldata2.extend_from_slice(&arg2);
                        match rpc.call(pool, Bytes::from(calldata2), block).await {
                            Ok(result) if result.0.len() >= 32 => {
                                let addr = Address::from_slice(&result.0[12..32]);
                                if addr.is_zero() { break; }
                                addr
                            }
                            _ => break,
                        }
                    }
                }
            };

            let balance = {
                let mut calldata = Vec::with_capacity(36);
                calldata.extend_from_slice(&CURVE_BALANCES_SELECTOR);
                let mut arg = [0u8; 32];
                arg[31] = i;
                calldata.extend_from_slice(&arg);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                    }
                    _ => break,
                }
            };

            tokens.push(token);
            balances.push(balance);
        }

        if tokens.len() < 2 {
            return None;
        }

        // Fetch amplification coefficient A() / get_A()
        let a_coeff = {
            // Try get_A() first (CryptoSwap V2), fallback to A() (StableSwap V1)
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_GET_A_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                }
                _ => {
                    // Fallback to A()
                    let mut calldata2 = Vec::with_capacity(4);
                    calldata2.extend_from_slice(&CURVE_A_SELECTOR);
                    match rpc.call(pool, Bytes::from(calldata2), block).await {
                        Ok(result) if result.0.len() >= 32 => {
                            U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                        }
                        _ => 100,
                    }
                }
            }
        };

        // Fetch swap fee fee()
        let fee_bps = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_FEE_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let chain_fee = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    (chain_fee / 10_000) as u32
                }
                _ => 0,
            }
        };

        // --- Variant detection ---
        // Try gamma() �?� only CryptoSwap V2 pools have this
        let is_crypto = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_GAMMA_SELECTOR);
            matches!(rpc.call(pool, Bytes::from(calldata), block).await, Ok(r) if r.0.len() >= 32 && !r.0[..32].iter().all(|&b| b == 0))
        };

        // Try base_pool() �?� only Metapools have this
        let base_pool = if !is_crypto {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_BASE_POOL_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let addr = Address::from_slice(&result.0[12..32]);
                    if !addr.is_zero() { Some(addr) } else { None }
                }
                _ => None,
            }
        } else {
            None
        };

        // Fetch variant-specific fields
        let (variant, gamma, price_scale) = if is_crypto {
            // Gamma
            let gamma_val = {
                let mut calldata = Vec::with_capacity(4);
                calldata.extend_from_slice(&CURVE_GAMMA_SELECTOR);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        Some(U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128)
                    }
                    _ => None,
                }
            };

            // Price scale �?� returns a dynamic array of N-1 values
            let price_scales = {
                let mut calldata = Vec::with_capacity(4);
                calldata.extend_from_slice(&CURVE_PRICE_SCALE_SELECTOR);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 64 => {
                        let off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                        let count = if off + 32 <= result.0.len() {
                            U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as usize
                        } else { 0 };
                        let start = off + 32;
                        let mut scales = Vec::with_capacity(count);
                        for j in 0..count {
                            let pos = start + j * 32;
                            if pos + 32 <= result.0.len() {
                                scales.push(U256::from_be_slice(&result.0[pos..pos + 32]).as_limbs()[0] as u128);
                            }
                        }
                        scales
                    }
                    _ => vec![],
                }
            };

            (CurvePoolVariant::Crypto, gamma_val, price_scales)
        } else if base_pool.is_some() {
            (CurvePoolVariant::Meta, None, vec![])
        } else {
            (CurvePoolVariant::Plain, None, vec![])
        };

        Some(PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool))
    }
}

