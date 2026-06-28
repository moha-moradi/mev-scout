use alloy::primitives::{Address, U256};

use crate::data::TxData;
use crate::mev::detectors::MultiHopArbDetector;
use crate::mev::detectors::TwoHopArbDetector;
use crate::types::MevOpportunity;
use crate::pool::math::constant_product_output_amount;
use crate::pool::state::{PoolManager, PoolState};
use crate::rpc::RpcClient;
use crate::types::GasConfig;
use crate::utils::u128_from_be_bytes;

/// Result of capturing the pending block from the mempool.
#[derive(Debug, Clone)]
pub struct PendingBlockCapture {
    pub block_number: u64,
    pub txs: Vec<TxData>,
    pub tx_count: usize,
    pub base_fee_per_gas: u128,
    pub timestamp: u64,
}

/// Capture the current pending block from the node's mempool.
///
/// Calls `eth_getBlockByNumber("pending", true)` to retrieve all pending
/// (not-yet-mined) transactions. Returns `None` if the RPC call fails or
/// the pending block is unavailable (some nodes disable this).
///
/// The returned txs can be used for informational display or merged with
/// settled transactions for extended MEV detection (Phase 2+).
pub async fn capture_pending_block(rpc: &RpcClient) -> Option<PendingBlockCapture> {
    let (block_data, txs) = rpc.get_pending_block().await.ok()?;
    let tx_count = txs.len();
    tracing::info!(
        "Captured pending block: {} pending transactions (block #{})",
        tx_count,
        block_data.number,
    );
    Some(PendingBlockCapture {
        block_number: block_data.number,
        txs,
        tx_count,
        base_fee_per_gas: block_data.base_fee_per_gas.unwrap_or(0),
        timestamp: block_data.timestamp,
    })
}

/// Run two-hop and multi-hop arbitrage detection against the current `PoolManager`
/// state to find opportunities visible in the mempool (pending/unconfirmed txs).
///
/// Unlike settled blocks, pending txs have no execution logs, so detectors that
/// require log parsing (JIT, Sandwich, JitArb, Liquidation) are skipped. Only
/// pool-state-based arbitrage detection is performed.
///
/// Returns opportunities with `mempool_only = true`.
pub fn detect_pending_opportunities(
    pool_manager: &PoolManager,
    gas_config: GasConfig,
    base_fee_per_gas: u128,
    timestamp: u64,
    block_number: u64,
) -> Vec<MevOpportunity> {
    let mut two_hop = TwoHopArbDetector::new(block_number);
    let mut multi_hop = MultiHopArbDetector::new(block_number);

    let mut results = Vec::new();

    // Run two-hop detection on current pool state
    let two_hop_opps = two_hop.detect(
        pool_manager,
        0,
        timestamp,
        base_fee_per_gas,
        gas_config,
    );
    results.extend(two_hop_opps);

    // Run multi-hop detection on current pool state
    let multi_hop_opps = multi_hop.detect(
        pool_manager,
        0,
        timestamp,
        base_fee_per_gas,
        gas_config,
    );
    results.extend(multi_hop_opps);

    // Label all as mempool-only
    for opp in &mut results {
        opp.mempool_only = true;
    }

    results
}

/// A single pool-side effect estimated from a pending transaction.
#[derive(Debug, Clone)]
pub struct PendingPoolEffect {
    /// Pool address whose state is affected.
    pub pool_address: Address,
    /// Token address whose balance/reserve changes.
    pub token_address: Address,
    /// Estimated change in the token's reserve (positive = increase, negative = decrease).
    pub reserve_delta: i128,
}

/// Run a pending tx through RPC simulation (`eth_call`) and estimate its
/// pool impact via calldata parsing.
///
/// This is the primary approach: first identify pool interactions via
/// calldata parsing, then validate the tx doesn't revert via `eth_call`
/// against the node's EVM (which uses an RPC-forked state at the given
/// block). If validation passes, the calldata-estimated pool effects
/// are returned. If the tx reverts or no DEX interaction is detected,
/// returns an empty vec.
///
/// The caller should fall back to [`estimate_pending_tx_pool_impact`]
/// (calldata-only, no eth_call validation) if this returns empty
/// and the RPC is rate-limited or unavailable.
pub async fn simulate_pending_tx_pool_impact(
    tx: &TxData,
    pool_manager: &PoolManager,
    rpc: &RpcClient,
    _chain_id: u64,
    block_number: u64,
) -> Vec<PendingPoolEffect> {
    // Step 1: Identify pool interactions via calldata parsing
    let effects = estimate_pending_tx_pool_impact(tx, pool_manager);
    if effects.is_empty() {
        return Vec::new();
    }

    // Step 2: Validate the tx doesn't revert via eth_call
    // Use block_number (parent of pending block) as the state to simulate against
    let sim_block = block_number.saturating_sub(1).max(1);
    match tx.to {
        Some(to) => {
            match rpc.call(to, tx.input.clone(), sim_block).await {
                Ok(ref result) if !result.is_empty() || true => {
                    // Tx succeeded — return estimated pool effects
                    effects
                }
                _ => {
                    // Tx reverted or call failed — no pool effects
                    Vec::new()
                }
            }
        }
        None => {
            // Contract creation — has no pool effects from calldata parsing
            Vec::new()
        }
    }
}

// ── ABI decoding helpers ───────────────────────────────────────────

/// Decode a u128 from a 32-byte ABI word (right-aligned).
fn abi_decode_u128(data: &[u8], offset: usize) -> Option<u128> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(u128_from_be_bytes(&data[offset..offset + 32]))
}

/// Decode an Address from a 32-byte ABI word (right-aligned).
fn abi_decode_address(data: &[u8], offset: usize) -> Option<Address> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(Address::from_slice(&data[offset + 12..offset + 32]))
}

/// Decode a U256 from a 32-byte ABI word.
fn abi_decode_u256(data: &[u8], offset: usize) -> Option<U256> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(U256::from_be_slice(&data[offset..offset + 32]))
}

// ── V2 calldata parsing ────────────────────────────────────────────

/// Parameters from a V2 `swapExactTokensForTokens` calldata.
struct V2ExactInParams {
    amount_in: u128,
    /// Token addresses in the path (path[0] = token in, path[last] = token out).
    path: Vec<Address>,
}

/// Parse `swapExactTokensForTokens(uint256,uint256,address[],address,uint256)`.
/// Selector: 0x38ed1739
fn parse_v2_swap_exact_tokens_for_tokens(data: &[u8]) -> Option<V2ExactInParams> {
    // selector(4) + amountIn(32) + amountOutMin(32) + pathOffset(32) = 100 bytes minimum
    if data.len() < 100 {
        return None;
    }
    let amount_in = abi_decode_u128(data, 4)?;
    let path_offset = abi_decode_u256(data, 68)?;
    let path_offset = path_offset.as_limbs()[0] as usize;
    let path_data_start = 4 + path_offset;
    if path_data_start + 32 > data.len() {
        return None;
    }
    let path_len = abi_decode_u256(data, path_data_start)?;
    let path_len = path_len.as_limbs()[0] as usize;
    if path_len < 2 {
        return None;
    }
    let mut path = Vec::with_capacity(path_len);
    for i in 0..path_len {
        let addr_offset = path_data_start + 32 + i * 32;
        path.push(abi_decode_address(data, addr_offset)?);
    }
    Some(V2ExactInParams { amount_in, path })
}

/// Parameters from a V2 `swapTokensForExactTokens` calldata.
struct V2ExactOutParams {
    amount_out: u128,
    path: Vec<Address>,
}

/// Parse `swapTokensForExactTokens(uint256,uint256,address[],address,uint256)`.
/// Selector: 0x8803dbee
fn parse_v2_swap_tokens_for_exact_tokens(data: &[u8]) -> Option<V2ExactOutParams> {
    if data.len() < 100 {
        return None;
    }
    let amount_out = abi_decode_u128(data, 4)?;
    let path_offset = abi_decode_u256(data, 68)?;
    let path_offset = path_offset.as_limbs()[0] as usize;
    let path_data_start = 4 + path_offset;
    if path_data_start + 32 > data.len() {
        return None;
    }
    let path_len = abi_decode_u256(data, path_data_start)?;
    let path_len = path_len.as_limbs()[0] as usize;
    if path_len < 2 {
        return None;
    }
    let mut path = Vec::with_capacity(path_len);
    for i in 0..path_len {
        let addr_offset = path_data_start + 32 + i * 32;
        path.push(abi_decode_address(data, addr_offset)?);
    }
    Some(V2ExactOutParams { amount_out, path })
}

/// For a consecutive pair of tokens in the path, find the V2 pool and estimate
/// the output amount given an input. Returns (output_amount, pool_address) or None.
fn estimate_v2_hop_output(
    pool_manager: &PoolManager,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
) -> Option<(u128, Address)> {
    let pool_addr = pool_manager.find_pair_pool(&token_in, &token_out)?;
    let pool = pool_manager.get(&pool_addr)?;
    match pool {
        PoolState::UniswapV2(v2) => {
            let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                (v2.reserve0, v2.reserve1)
            } else {
                (v2.reserve1, v2.reserve0)
            };
            let output = constant_product_output_amount(amount_in, reserve_in, reserve_out, v2.info.fee)?;
            Some((output, pool_addr))
        }
        _ => {
            // V3 pool found for V2 path — still try to quote it
            let output = crate::pool::math::quote_exact_in(pool, token_in, token_out, amount_in)?;
            Some((output, pool_addr))
        }
    }
}

/// Estimate the pool reserve changes for a single V2 swap hop.
/// Returns two effects: one for each token in the pool.
fn v2_swap_effects(
    pool_addr: Address,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
    amount_out: u128,
) -> Vec<PendingPoolEffect> {
    vec![
        PendingPoolEffect {
            pool_address: pool_addr,
            token_address: token_in,
            reserve_delta: amount_in as i128,
        },
        PendingPoolEffect {
            pool_address: pool_addr,
            token_address: token_out,
            reserve_delta: -(amount_out as i128),
        },
    ]
}

/// Estimate effects for a V2 exact-in swap (swapExactTokensForTokens).
fn estimate_v2_exact_in(data: &[u8], pool_manager: &PoolManager) -> Vec<PendingPoolEffect> {
    let params = match parse_v2_swap_exact_tokens_for_tokens(data) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let path = &params.path;
    let mut effects = Vec::new();
    let mut current_amount = params.amount_in;

    for window in path.windows(2) {
        let token_in = window[0];
        let token_out = window[1];
        match estimate_v2_hop_output(pool_manager, token_in, token_out, current_amount) {
            Some((output, pool_addr)) => {
                effects.extend(v2_swap_effects(pool_addr, token_in, token_out, current_amount, output));
                current_amount = output;
            }
            None => break,
        }
    }

    effects
}

/// For a consecutive pair in the path, find the V2 pool and estimate the
/// input amount needed for a given output. Returns (input_amount, pool_address).
fn estimate_v2_hop_input(
    pool_manager: &PoolManager,
    token_in: Address,
    token_out: Address,
    amount_out: u128,
) -> Option<(u128, Address)> {
    let pool_addr = pool_manager.find_pair_pool(&token_in, &token_out)?;
    let pool = pool_manager.get(&pool_addr)?;
    match pool {
        PoolState::UniswapV2(v2) => {
            let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                (v2.reserve0, v2.reserve1)
            } else {
                (v2.reserve1, v2.reserve0)
            };
            let input = crate::pool::math::constant_product_input_amount(amount_out, reserve_in, reserve_out, v2.info.fee)?;
            Some((input, pool_addr))
        }
        _ => None,
    }
}

/// Estimate effects for a V2 exact-out swap (swapTokensForExactTokens).
fn estimate_v2_exact_out(data: &[u8], pool_manager: &PoolManager) -> Vec<PendingPoolEffect> {
    let params = match parse_v2_swap_tokens_for_exact_tokens(data) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let path = &params.path;
    // Walk backwards: from token_out to token_in, computing required inputs
    let mut effects = Vec::new();
    let mut remaining_out = params.amount_out;

    // Build effects backwards (last hop first)
    let mut hop_effects: Vec<Vec<PendingPoolEffect>> = Vec::new();
    for window in path.windows(2).rev() {
        let token_in = window[0];
        let token_out = window[1];
        match estimate_v2_hop_input(pool_manager, token_in, token_out, remaining_out) {
            Some((input, pool_addr)) => {
                hop_effects.push(v2_swap_effects(pool_addr, token_in, token_out, input, remaining_out));
                remaining_out = input;
            }
            None => return Vec::new(),
        }
    }

    // Reverse effects to be in forward order
    for h in hop_effects.into_iter().rev() {
        effects.extend(h);
    }

    effects
}

// ── V3 calldata parsing ────────────────────────────────────────────

/// Decode a V3 path into token+pool segments.
/// Path format: tokenIn (20 bytes) + fee (3 bytes) + tokenOut (20 bytes) per hop.
struct V3Hop {
    token_in: Address,
    token_out: Address,
    fee: u32,
}

/// Parse the V3 packed path bytes into individual hops.
/// Format per hop: tokenIn(20) + fee(3) + tokenOut(20) = 43 bytes
fn parse_v3_path(path_bytes: &[u8]) -> Vec<V3Hop> {
    if path_bytes.len() < 43 || (path_bytes.len() - 20) % 23 != 0 {
        return Vec::new();
    }
    let mut hops = Vec::new();
    let mut offset = 0;
    while offset + 43 <= path_bytes.len() {
        let token_in = Address::from_slice(&path_bytes[offset..offset + 20]);
        let fee_bytes: [u8; 4] = [0, path_bytes[offset + 20], path_bytes[offset + 21], path_bytes[offset + 22]];
        let fee = u32::from_be_bytes(fee_bytes);
        let token_out = Address::from_slice(&path_bytes[offset + 23..offset + 43]);
        hops.push(V3Hop { token_in, token_out, fee });
        // For the next hop: the current token_out becomes the next token_in
        offset += 23;
    }
    hops
}

/// Parse `exactInput((bytes,address,uint256,uint256,uint256))`.
/// Selector: 0xc04b8d59
fn parse_v3_exact_input(data: &[u8]) -> Option<(Vec<V3Hop>, u128)> {
    if data.len() < 36 {
        return None;
    }
    let tuple_offset = abi_decode_u256(data, 4)?;
    let tuple_offset = tuple_offset.as_limbs()[0] as usize;
    let struct_start = 4 + tuple_offset;
    if struct_start + 160 > data.len() {
        return None;
    }
    // pathOffset is first field in the struct (dynamic: bytes)
    let path_offset = abi_decode_u256(data, struct_start)?;
    let path_offset = path_offset.as_limbs()[0] as usize;
    let path_data_start = struct_start + path_offset;
    if path_data_start + 32 > data.len() {
        return None;
    }
    let path_len = abi_decode_u256(data, path_data_start)?;
    let path_len = path_len.as_limbs()[0] as usize;
    let path_bytes_start = path_data_start + 32;
    if path_bytes_start + path_len > data.len() {
        return None;
    }
    let path_bytes = &data[path_bytes_start..path_bytes_start + path_len];
    let hops = parse_v3_path(path_bytes);
    if hops.is_empty() {
        return None;
    }
    // amountIn is at struct_start + 96 (4th field: offset 0=path, 32=recipient, 64=deadline, 96=amountIn)
    let amount_in = abi_decode_u128(data, struct_start + 96)?;
    Some((hops, amount_in))
}

/// Estimate effects for a V3 exactInput swap.
fn estimate_v3_exact_in(data: &[u8], pool_manager: &PoolManager) -> Vec<PendingPoolEffect> {
    let (hops, amount_in) = match parse_v3_exact_input(data) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut effects = Vec::new();
    let mut current_amount = amount_in;
    for hop in &hops {
        let pool_addr = pool_manager.find_pair_pool(&hop.token_in, &hop.token_out);
        let pool_addr = match pool_addr {
            Some(a) => a,
            None => break,
        };
        // Verify this is a V3 pool with matching fee
        let is_v3 = match pool_manager.get(&pool_addr) {
            Some(PoolState::UniswapV3(v3)) => v3.info.fee == hop.fee,
            _ => false,
        };
        if !is_v3 {
            // Fallback: use any pool for the pair
        }
        // For V3, we estimate output via sqrt price if available, otherwise skip
        let output = match pool_manager.get(&pool_addr) {
            Some(PoolState::UniswapV3(v3)) => {
                let zero_for_one = v3.info.token0 == hop.token_in;
                crate::pool::math::v3::quote_v3_exact_in(v3, current_amount, zero_for_one)
            }
            Some(pool) => crate::pool::math::quote_exact_in(pool, hop.token_in, hop.token_out, current_amount),
            None => None,
        };
        let output = match output {
            Some(o) => o,
            None => break,
        };
        effects.extend(v2_swap_effects(pool_addr, hop.token_in, hop.token_out, current_amount, output));
        current_amount = output;
    }
    effects
}

/// Parse `exactOutput((bytes,address,uint256,uint256,uint256))`.
/// Selector: 0xf28c0498
#[allow(dead_code)]
fn parse_v3_exact_output(data: &[u8]) -> Option<(Vec<V3Hop>, u128)> {
    if data.len() < 36 {
        return None;
    }
    let tuple_offset = abi_decode_u256(data, 4)?;
    let tuple_offset = tuple_offset.as_limbs()[0] as usize;
    let struct_start = 4 + tuple_offset;
    if struct_start + 160 > data.len() {
        return None;
    }
    let path_offset = abi_decode_u256(data, struct_start)?;
    let path_offset = path_offset.as_limbs()[0] as usize;
    let path_data_start = struct_start + path_offset;
    if path_data_start + 32 > data.len() {
        return None;
    }
    let path_len = abi_decode_u256(data, path_data_start)?;
    let path_len = path_len.as_limbs()[0] as usize;
    let path_bytes_start = path_data_start + 32;
    if path_bytes_start + path_len > data.len() {
        return None;
    }
    let path_bytes = &data[path_bytes_start..path_bytes_start + path_len];
    let hops = parse_v3_path(path_bytes);
    if hops.is_empty() {
        return None;
    }
    // amountOut is at struct_start + 96 (4th field: 0=path, 32=recipient, 64=deadline, 96=amountOut)
    let amount_out = abi_decode_u128(data, struct_start + 96)?;
    Some((hops, amount_out))
}

/// Estimate effects for a V3 exactOutput swap (walk path backwards).
/// Note: exact-output estimation is inherently approximate; V3 exact-output
/// effects are omitted to avoid misleading estimates.
fn estimate_v3_exact_out(_data: &[u8], _pool_manager: &PoolManager) -> Vec<PendingPoolEffect> {
    Vec::new()
}

/// Estimate the effect of a single pending tx on pool reserves
/// by parsing known DEX router calldata patterns (fallback when revm
/// simulation is too expensive or unavailable).
///
/// Supports:
/// - V2: `swapExactTokensForTokens`, `swapTokensForExactTokens`
/// - V3: `exactInput`, `exactOutput` path decoding (partial)
///
/// If parsing fails or the pool is unknown, returns an empty vec.
pub fn estimate_pending_tx_pool_impact(
    tx: &TxData,
    pool_manager: &PoolManager,
) -> Vec<PendingPoolEffect> {
    let data = tx.input.as_ref();
    if data.len() < 4 {
        return Vec::new();
    }
    let selector = &data[..4];
    // V2: swapExactTokensForTokens
    if selector == [0x38, 0xed, 0x17, 0x39] {
        return estimate_v2_exact_in(data, pool_manager);
    }
    // V2: swapTokensForExactTokens
    if selector == [0x88, 0x03, 0xdb, 0xee] {
        return estimate_v2_exact_out(data, pool_manager);
    }
    // V3: exactInput
    if selector == [0xc0, 0x4b, 0x8d, 0x59] {
        return estimate_v3_exact_in(data, pool_manager);
    }
    // V3: exactOutput
    if selector == [0xf2, 0x8c, 0x04, 0x98] {
        return estimate_v3_exact_out(data, pool_manager);
    }
    Vec::new()
}
