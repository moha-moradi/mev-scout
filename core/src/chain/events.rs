//! Event topic constants and log decoders for on-chain scanning.
//!
//! Centralizes all ERC-20, DEX, flash loan, and liquidation event signatures
//! so scanner modules reference a single source of truth.

use alloy::primitives::{b256, keccak256, Address, B256, I256, U256};
use alloy::rpc::types::Log;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

// ── ERC-20 ──────────────────────────────────────────────────────────

pub const TRANSFER_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

// ── Uniswap V2 ──────────────────────────────────────────────────────

pub const V2_SWAP_TOPIC: B256 =
    b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

// ── Uniswap V3 ──────────────────────────────────────────────────────

pub const V3_SWAP_TOPIC: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

pub static V3_FLASH_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Flash(address,address,uint256,uint256,bytes)")
});

// ── Uniswap V4 ──────────────────────────────────────────────────────

/// Uniswap V4 PoolManager Swap event (verified against v4-core PoolManager):
/// `emit Swap(id, msg.sender, delta.amount0(), delta.amount1(), sqrtPriceX96,
///             liquidity, tick, swapFee)` with `id: PoolId (bytes32)` in
/// topics[1]. NOTE: this is NOT the same signature as the V3 Swap event —
/// the previous string here was identical to V3's, hashing to the V3 topic
/// and silently disabling V4 trade detection.
pub static V4_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)")
});

// ── Pancake Infinity CL ──────────────────────────────────────────────

/// Pancake Infinity CLPoolManager Swap event (verified against
/// pancakeswap/infinity-core CLPoolManager.sol + ICLPoolManager.sol):
/// `emit Swap(id, msg.sender, delta.amount0(), delta.amount1(), sqrtPriceX96,
///             liquidity, tick, fee, protocolFee)` with `id: PoolId (bytes32)`
/// in topics[1]. The singleton CLPoolManager emits for every pool; the pool
/// key is the bytes32 PoolId (synthetic pool address = its first 20 bytes,
/// mirroring `discovery/infinity.rs`).
pub static INF_CL_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24,uint16)")
});

// ── Fluid DEX ────────────────────────────────────────────────────────

/// Fluid DEX pool Swap event (verified against Instadapp/fluid-contracts-public
/// poolT1/coreModule/events.sol): `emit Swap(swap0to1, amountIn, amountOut, to)`
/// — no indexed params, everything is in the data words. Emitted by each
/// per-pool `Dex` contract; reserves live in the shared Liquidity layer.
pub static FLUID_DEX_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(bool,uint256,uint256,address)"));

/// Fluid DEX factory pool-creation event (verified against the same repo's
/// factory/main.sol): `emit LogDexDeployed(dex, dexId)` with both params
/// indexed. Consumed by `discovery/fluid.rs`; kept here for completeness.
pub static FLUID_DEX_DEPLOYED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("LogDexDeployed(address,uint256)"));

// ── Metric V2 ────────────────────────────────────────────────────────

/// Metric V2 pool Swap event (plan §3.4 signature): `Swap(address sender,
/// address recipient, bool exactInput, int128 amount0Delta, int128 amount1Delta,
/// int16 newTick, uint104 newPositionInBin)` with sender/recipient assumed
/// indexed. Topic digest computed from the signature string; on-chain
/// verification deferred (same class as Q6/Q10/Q11).
pub static METRIC_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(address,address,bool,int128,int128,int16,uint104)")
});

// ── Balancer V2 ─────────────────────────────────────────────────────

pub static BALANCER_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,bytes)"));

// ── Aave V2 ─────────────────────────────────────────────────────────

pub static AAVE_V2_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,uint256,uint16)"));

// ── Aave V3 ─────────────────────────────────────────────────────────

pub static AAVE_V3_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,uint8,uint256,uint16)"));

pub static AAVE_V3_LIQUIDATION_CALL_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)")
});

// ── Compound V3 ─────────────────────────────────────────────────────

pub static COMPOUND_V3_ABSORB_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Absorb(address,address[],uint256[],uint256)"));

// ── Solidly / Velodrome / Aerodrome ─────────────────────────────────

/// Velodrome V2/Aerodrome pool Swap topic (`Swap(address indexed sender,
/// address indexed to, uint256 amount0In, uint256 amount1In, uint256
/// amount0Out, uint256 amount1Out)` — verified against velodrome-finance/
/// contracts `IPool.sol`). Original Solidly V1 / Camelot pairs instead emit
/// the Uniswap V2 signature, covered by `V2_SWAP_TOPIC`.
pub static SOLIDLY_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,uint256,uint256,uint256,uint256)"));

/// Decode a Solidly/Velodrome/Aerodrome Swap event.
///
/// Topics: [sig, sender, to]; data carries
/// `[amount0In, amount1In, amount0Out, amount1Out]` = 128 bytes — the same
/// four-amount layout as Uniswap V2's Swap data. Exactly one of the in-words
/// and one of the out-words is nonzero for a normal swap.
pub fn decode_solidly_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 128 {
        return None;
    }
    let a0i = U256::from_be_slice(&data[0..32]);
    let a1i = U256::from_be_slice(&data[32..64]);
    let a0o = U256::from_be_slice(&data[64..96]);
    let a1o = U256::from_be_slice(&data[96..128]);
    if a0i.is_zero() && a1i.is_zero() && a0o.is_zero() && a1o.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in: a0i.max(a1i),
        amount_out: a0o.max(a1o),
        dex_type: "solidly".to_string(),
    })
}

// ── Balancer V2 ───────────────────────────────────────────────────

/// Balancer V2 Vault Swap event: `Swap(bytes32 indexed poolId, address
/// indexed tokenIn, address indexed tokenOut, uint256 amountIn, uint256
/// amountOut)`. Hash verified against the Balancer V2 Vault contract.
pub static BALANCER_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    crate::pool::decoders::BALANCER_SWAP_TOPIC.into()
});

/// Decode a Balancer V2 Vault Swap event.
///
/// The pool address is derived from the poolId (first 20 bytes of the
/// bytes32 poolId). Token addresses are extracted from indexed topics.
pub fn decode_balancer_swap(log: &Log) -> Option<TradeEvent> {
    let topics = log.topics();
    if topics.len() < 4 || topics[0] != *BALANCER_SWAP_TOPIC {
        return None;
    }
    let data = &log.data().data;
    if data.len() < 64 {
        return None;
    }
    // poolId = first 20 bytes are the pool address, last 12 bytes are specialization
    let pool = Address::from_slice(&topics[1][12..]);
    let token_in = Address::from_slice(&topics[2][12..]);
    let token_out = Address::from_slice(&topics[3][12..]);
    let amount_in = U256::from_be_slice(&data[0..32]);
    let amount_out = U256::from_be_slice(&data[32..64]);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in,
        token_out,
        amount_in,
        amount_out,
        dex_type: "balancer".to_string(),
    })
}

// ── Curve ───────────────────────────────────────────────────────────

pub static CURVE_TOKEN_EXCHANGE_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256)"));

pub static CURVE_V2_TOKEN_EXCHANGE_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256,uint256)"));

// ── Trader Joe LB ───────────────────────────────────────────────────

/// Trader Joe Liquidity Book 2.0 Pair Swap event (also matches the LB 2.2
/// per-side-amounts form — same canonical signature). Verified against
/// lfj-gg/joe-v2 branch v2.0.
pub static TRADER_JOE_LB_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,uint256,bool,uint256,uint256,uint256,uint256)"));

/// Trader Joe Liquidity Book 2.1/2.2 Pair Swap event (packed bytes32 amounts).
/// Verified against lfj-gg/joe-v2 branches main/v2.1/v2.2.
pub static TRADER_JOE_LB_SWAP_LEGACY_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(address,address,uint24,bytes32,bytes32,uint24,bytes32,bytes32)")
});

// ── Pendle Finance ──────────────────────────────────────────────────

/// Pendle V2 market Swap event (caller, receiver indexed).
pub static PENDLE_MARKET_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,int256,int256,uint256,uint256)"));

// ── Decoded event structs ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashLoanEvent {
    pub block: u64,
    pub tx_hash: B256,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub protocol: String,
    pub initiator: Address,
    pub target: Address,
    pub token: Address,
    pub amount: U256,
    pub fee: Option<U256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationEvent {
    pub block: u64,
    pub tx_hash: B256,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub protocol: String,
    pub user: Address,
    pub liquidator: Address,
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub collateral_amount: U256,
    pub debt_to_cover: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEvent {
    pub block: u64,
    pub tx_hash: B256,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
    pub dex_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEvent {
    pub block: u64,
    pub tx_hash: B256,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub value: U256,
}

// ── Log decoders ────────────────────────────────────────────────────

/// Decode an ERC-20 Transfer event log.
pub fn decode_transfer(log: &Log) -> Option<TransferEvent> {
    let topics = log.topics();
    if topics.len() < 3 || topics[0] != TRANSFER_TOPIC {
        return None;
    }
    // Malformed/rare logs can carry a data payload shorter than one word
    // (e.g. some aggregators); treat them as undecodable instead of panicking.
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    let from = Address::from_slice(&topics[1][12..]);
    let to = Address::from_slice(&topics[2][12..]);
    let value = U256::from_be_slice(&data[0..32]);
    Some(TransferEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        token: log.address(),
        from,
        to,
        value,
    })
}

/// Decode an Aave V3 FlashLoan event log.
pub fn decode_aave_v3_flash(log: &Log) -> Option<FlashLoanEvent> {
    let topics = log.topics();
    if topics.is_empty() || topics[0] != *AAVE_V3_FLASH_LOAN_TOPIC {
        return None;
    }
    let initiator = if topics.len() > 1 {
        Address::from_slice(&topics[1][12..])
    } else {
        Address::ZERO
    };
    let target = if topics.len() > 2 {
        Address::from_slice(&topics[2][12..])
    } else {
        Address::ZERO
    };
    let data = &log.data().data;
    let token = if data.len() >= 32 {
        Address::from_slice(&data[0..20])
    } else {
        Address::ZERO
    };
    let amount = if data.len() >= 64 {
        U256::from_be_slice(&data[32..64])
    } else {
        return None;
    };
    let fee = if data.len() >= 96 {
        Some(U256::from_be_slice(&data[64..96]))
    } else {
        None
    };
    Some(FlashLoanEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        protocol: "aave_v3".to_string(),
        initiator,
        target,
        token,
        amount,
        fee,
    })
}

/// Decode an Aave V2 FlashLoan event log.
pub fn decode_aave_v2_flash(log: &Log) -> Option<FlashLoanEvent> {
    let topics = log.topics();
    if topics.is_empty() || topics[0] != *AAVE_V2_FLASH_LOAN_TOPIC {
        return None;
    }
    let initiator = if topics.len() > 1 {
        Address::from_slice(&topics[1][12..])
    } else {
        Address::ZERO
    };
    let target = if topics.len() > 2 {
        Address::from_slice(&topics[2][12..])
    } else {
        Address::ZERO
    };
    let data = &log.data().data;
    let token = if data.len() >= 32 {
        Address::from_slice(&data[0..20])
    } else {
        Address::ZERO
    };
    let amount = if data.len() >= 64 {
        U256::from_be_slice(&data[32..64])
    } else {
        return None;
    };
    let fee = if data.len() >= 96 {
        Some(U256::from_be_slice(&data[64..96]))
    } else {
        None
    };
    Some(FlashLoanEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        protocol: "aave_v2".to_string(),
        initiator,
        target,
        token,
        amount,
        fee,
    })
}

/// Decode a Balancer V2 Vault FlashLoan event log.
pub fn decode_balancer_flash(log: &Log) -> Option<FlashLoanEvent> {
    let topics = log.topics();
    if topics.is_empty() || topics[0] != *BALANCER_FLASH_LOAN_TOPIC {
        return None;
    }
    let caller = if topics.len() > 1 {
        Address::from_slice(&topics[1][12..])
    } else {
        Address::ZERO
    };
    let recipient = if topics.len() > 2 {
        Address::from_slice(&topics[2][12..])
    } else {
        Address::ZERO
    };
    let data = &log.data().data;
    let token = if data.len() >= 20 {
        Address::from_slice(&data[0..20])
    } else {
        Address::ZERO
    };
    let amount = if data.len() >= 52 {
        U256::from_be_slice(&data[20..52])
    } else {
        return None;
    };
    Some(FlashLoanEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        protocol: "balancer_v2".to_string(),
        initiator: caller,
        target: recipient,
        token,
        amount,
        fee: None,
    })
}

/// Decode an Aave V3 LiquidationCall event log.
pub fn decode_aave_v3_liquidation(log: &Log) -> Option<LiquidationEvent> {
    let topics = log.topics();
    if topics.is_empty() || topics[0] != *AAVE_V3_LIQUIDATION_CALL_TOPIC {
        return None;
    }
    let collateral_asset = if topics.len() > 1 {
        Address::from_slice(&topics[1][12..])
    } else {
        Address::ZERO
    };
    let debt_asset = if topics.len() > 2 {
        Address::from_slice(&topics[2][12..])
    } else {
        Address::ZERO
    };
    let user = if topics.len() > 3 {
        Address::from_slice(&topics[3][12..])
    } else {
        Address::ZERO
    };
    let data = &log.data().data;
    let liquidator = if data.len() >= 20 {
        Address::from_slice(&data[0..20])
    } else {
        Address::ZERO
    };
    let debt_to_cover = if data.len() >= 52 {
        U256::from_be_slice(&data[20..52])
    } else {
        U256::ZERO
    };
    let collateral_amount = if data.len() >= 84 {
        U256::from_be_slice(&data[52..84])
    } else {
        U256::ZERO
    };
    Some(LiquidationEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        protocol: "aave_v3".to_string(),
        user,
        liquidator,
        collateral_asset,
        debt_asset,
        collateral_amount,
        debt_to_cover,
    })
}

/// Decode a Uniswap V3 (or V4) Swap event log.
///
/// V3 amounts are signed int256: amount < 0 means the pool received that
/// token (token in), amount > 0 means the pool paid out (token out). The
/// decoder reports magnitudes; the larger absolute value is `amount_in`.
pub fn decode_uniswap_v3_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 64 {
        return None;
    }
    let a0 = I256::try_from_be_slice(&data[0..32]).unwrap_or(I256::ZERO);
    let a1 = I256::try_from_be_slice(&data[32..64]).unwrap_or(I256::ZERO);
    if a0.is_zero() && a1.is_zero() {
        return None;
    }
    let abs0 = a0.wrapping_abs();
    let abs1 = a1.wrapping_abs();
    let (amount_in, amount_out) = if abs0 >= abs1 { (abs0, abs1) } else { (abs1, abs0) };
    let amount_in = U256::try_from(amount_in).unwrap_or(U256::ZERO);
    let amount_out = U256::try_from(amount_out).unwrap_or(U256::ZERO);
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "uniswap_v3".to_string(),
    })
}

/// Decode a Uniswap V4 PoolManager Swap event log.
///
/// V4 pools live inside the singleton PoolManager: `topics[1]` carries the
/// bytes32 poolId and the first two data words are signed int128 amounts.
/// The synthetic pool address is derived from the poolId exactly like the
/// Initialize-event scanner (`discovery/v4.rs`) so trade events join against
/// discovered pools.
pub fn decode_uniswap_v4_swap(log: &Log) -> Option<TradeEvent> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let pool_id: [u8; 32] = topics[1].0;
    let pool = Address::from_slice(&pool_id[12..32]);
    let data = &log.data().data;
    if data.len() < 64 {
        return None;
    }
    // amount0/amount1 are int128 (sign-extended into their 32-byte words);
    // one leg is typically negative — report magnitudes, larger as amount_in.
    let a0 = I256::try_from_be_slice(&data[0..32]).unwrap_or(I256::ZERO).wrapping_abs();
    let a1 = I256::try_from_be_slice(&data[32..64]).unwrap_or(I256::ZERO).wrapping_abs();
    let (amount_in, amount_out) = if a0 >= a1 { (a0, a1) } else { (a1, a0) };
    let amount_in = U256::try_from(amount_in).unwrap_or(U256::ZERO);
    let amount_out = U256::try_from(amount_out).unwrap_or(U256::ZERO);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "uniswap_v4".to_string(),
    })
}

/// Decode a Pancake Infinity CLPoolManager Swap event log.
///
/// Same singleton-manager shape as Uniswap V4: `topics[1]` carries the bytes32
/// PoolId (synthetic pool address = first 20 bytes) and the first two data
/// words are signed int128 net deltas. The remaining words (sqrtPriceX96,
/// liquidity, tick) match the V4 layout, so quoting reuses the CL math.
pub fn decode_infinity_cl_swap(log: &Log) -> Option<TradeEvent> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let pool_id: [u8; 32] = topics[1].0;
    let pool = Address::from_slice(&pool_id[12..32]);
    let data = &log.data().data;
    if data.len() < 64 {
        return None;
    }
    let a0 = I256::try_from_be_slice(&data[0..32]).unwrap_or(I256::ZERO).wrapping_abs();
    let a1 = I256::try_from_be_slice(&data[32..64]).unwrap_or(I256::ZERO).wrapping_abs();
    let (amount_in, amount_out) = if a0 >= a1 { (a0, a1) } else { (a1, a0) };
    let amount_in = U256::try_from(amount_in).unwrap_or(U256::ZERO);
    let amount_out = U256::try_from(amount_out).unwrap_or(U256::ZERO);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "pancake_infinity".to_string(),
    })
}

/// Decode a Fluid DEX pool Swap event log.
///
/// Event layout (verified against fluid-contracts-public): data = [swap0to1
/// (32B), amountIn (32B), amountOut (32B), to (32B)]. The pool address is the
/// emitting contract. Tokens are not in the event — the caller resolves them
/// from pool state.
pub fn decode_fluid_swap(log: &Log) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 96 {
        return None;
    }
    let amount_in = U256::from_be_slice(&data[32..64]);
    let amount_out = U256::from_be_slice(&data[64..96]);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool: log.address(),
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "fluid".to_string(),
    })
}

/// Decode a Metric V2 pool Swap event log.
///
/// Event layout (plan §3.4): topics = [sig, sender, recipient]; data = 5 words
/// [exactInput, int128 amount0Delta, int128 amount1Delta, int16 newTick,
/// uint104 newPositionInBin]. Like V3, one delta leg is negative — report
/// magnitudes, larger as amount_in.
pub fn decode_metric_swap(log: &Log) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 160 {
        return None;
    }
    let a0 = I256::try_from_be_slice(&data[32..64]).unwrap_or(I256::ZERO).wrapping_abs();
    let a1 = I256::try_from_be_slice(&data[64..96]).unwrap_or(I256::ZERO).wrapping_abs();
    let (amount_in, amount_out) = if a0 >= a1 { (a0, a1) } else { (a1, a0) };
    let amount_in = U256::try_from(amount_in).unwrap_or(U256::ZERO);
    let amount_out = U256::try_from(amount_out).unwrap_or(U256::ZERO);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool: log.address(),
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "metric".to_string(),
    })
}

/// Decode a Uniswap V2 Swap event log.
///
/// V2 event data: `amount0In, amount1In, amount0Out, amount1Out` — four
/// uint256 words. Exactly one In and one Out are non-zero per swap.
pub fn decode_uniswap_v2_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 128 {
        return None;
    }
    let a0i = U256::from_be_slice(&data[0..32]);
    let a1i = U256::from_be_slice(&data[32..64]);
    let a0o = U256::from_be_slice(&data[64..96]);
    let a1o = U256::from_be_slice(&data[96..128]);
    let amount_in = a0i.max(a1i);
    let amount_out = a0o.max(a1o);
    if amount_in.is_zero() && amount_out.is_zero() {
        return None;
    }
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "uniswap_v2".to_string(),
    })
}

/// Decode a Curve TokenExchange event log.
///
/// Data layout: `sold_id(int128), amount_sold(uint256), bought_id(int128),
/// amount_bought(uint256)`. The `amount_bought` (token out) is at offset 96.
pub fn decode_curve_exchange(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    let amount_in = if data.len() >= 64 {
        U256::from_be_slice(&data[32..64])
    } else {
        return None;
    };
    let amount_out = if data.len() >= 128 {
        U256::from_be_slice(&data[96..128])
    } else {
        U256::ZERO
    };
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "curve".to_string(),
    })
}

/// Decode a Trader Joe Liquidity Book Pair Swap event log.
///
/// LB 2.0 / 2.2 (`legacy = false`): data = swapForY(bool), amountIn,
/// amountOut, volatilityAccumulated, fees.
/// LB 2.1 (`legacy = true`): data = amountsIn(bytes32 packed X|Y),
/// amountsOut(bytes32 packed X|Y), volatilityAccumulator, totalFees,
/// protocolFees. Packed words hold two token halves; the dominant (larger)
/// half is reported — the event does not expose which side flowed.
pub fn decode_trader_joe_lb_swap(log: &Log, pool: Address, legacy: bool) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 96 {
        return None;
    }
    let (amount_in, amount_out) = if legacy {
        let unpack = |word: &[u8]| -> U256 {
            let hi = U256::from_be_slice(&word[0..16]);
            let lo = U256::from_be_slice(&word[16..32]);
            hi.max(lo)
        };
        (
            unpack(&data[0..32]),
            unpack(&data[32..64]),
        )
    } else {
        (U256::from_be_slice(&data[32..64]), U256::from_be_slice(&data[64..96]))
    };
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in,
        amount_out,
        dex_type: "trader_joe_lb".to_string(),
    })
}

/// Decode a Pendle market Swap event log.
///
/// Data: netPtToAccount(int256), netSyToAccount(int256), netSyFee(uint256),
/// netSyToReserve(uint256). The PT/SY legs are signed — report magnitudes.
pub fn decode_pendle_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    if data.len() < 64 {
        return None;
    }
    let pt = I256::try_from_be_slice(&data[0..32]).unwrap_or(I256::ZERO).wrapping_abs();
    let sy = I256::try_from_be_slice(&data[32..64]).unwrap_or(I256::ZERO).wrapping_abs();
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in: U256::try_from(pt).unwrap_or(U256::ZERO),
        amount_out: U256::try_from(sy).unwrap_or(U256::ZERO),
        dex_type: "pendle".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{b256, LogData};

    #[test]
    fn transfer_topic_is_correct() {
        assert_eq!(
            TRANSFER_TOPIC,
            b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
        );
    }

    #[test]
    fn v2_swap_topic_is_correct() {
        assert_eq!(
            V2_SWAP_TOPIC,
            b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")
        );
    }

    #[test]
    fn v3_swap_topic_is_correct() {
        assert_eq!(
            V3_SWAP_TOPIC,
            b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67")
        );
    }

    /// Solidly topic must be the verified Velodrome V2 signature and must
    /// NOT collide with the V2 topic (original Solidly V1 pairs share V2's).
    #[test]
    fn solidly_swap_topic_is_verified() {
        assert_eq!(
            *SOLIDLY_SWAP_TOPIC,
            b256!("b3e2773606abfd36b5bd91394b3a54d1398336c65005baf7bf7a05efeffaf75b")
        );
        assert_ne!(*SOLIDLY_SWAP_TOPIC, V2_SWAP_TOPIC);
    }

    /// Regression guard: the V4 Swap topic must NOT equal the V3 topic.
    /// The old string `Swap(address,address,int256,int256,uint160,uint128,int24)`
    /// hashed to exactly V3_SWAP, silently disabling V4 trade detection.
    #[test]
    fn v4_swap_topic_differs_from_v3_and_is_verified() {
        assert_ne!(*V4_SWAP_TOPIC, V3_SWAP_TOPIC);
        assert_eq!(
            *V4_SWAP_TOPIC,
            b256!("40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f")
        );
    }

    #[test]
    fn trader_joe_lb_topics_are_verified() {
        // LB 2.0 / 2.2 form
        assert_eq!(
            *TRADER_JOE_LB_SWAP_TOPIC,
            b256!("c528cda9e500228b16ce84fadae290d9a49aecb17483110004c5af0a07f6fd73")
        );
        // LB 2.1 packed-bytes32 form
        assert_eq!(
            *TRADER_JOE_LB_SWAP_LEGACY_TOPIC,
            b256!("ad7d6f97abf51ce18e17a38f4d70e975be9c0708474987bb3e26ad21bd93ca70")
        );
        assert_ne!(*TRADER_JOE_LB_SWAP_TOPIC, *TRADER_JOE_LB_SWAP_LEGACY_TOPIC);
    }

    fn make_log(address: Address, topics_vec: Vec<B256>, data_bytes: Vec<u8>) -> Log {
        let data = LogData::new_unchecked(topics_vec, alloy::primitives::Bytes::from(data_bytes));
        Log {
            inner: alloy::primitives::Log { address, data },
            block_number: Some(100),
            block_hash: None,
            block_timestamp: None,
            transaction_hash: Some(b256!("0000000000000000000000000000000000000000000000000000000000000001")),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        }
    }

    #[test]
    fn decode_uniswap_v4_swap_uses_pool_id_topic() {
        let pool_id = b256!("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef");
        // int128 legs are sign-extended across the FULL 256-bit word: the
        // upper half must be 0xff for negatives, mirroring real PoolManager logs.
        let mut amount0 = [0xffu8; 32];
        amount0[16..32].copy_from_slice(&(-5_000i128).to_be_bytes());
        let mut amount1 = [0u8; 32];
        amount1[16..32].copy_from_slice(&2_500i128.to_be_bytes());

        let log = make_log(
            Address::ZERO, // PoolManager singleton — not a pool address
            vec![*V4_SWAP_TOPIC, pool_id],
            [amount0, amount1].concat(),
        );

        let evt = decode_uniswap_v4_swap(&log).unwrap();
        assert_eq!(evt.dex_type, "uniswap_v4");
        // Synthetic pool key = last 20 bytes of poolId (consistent with discovery/v4.rs)
        assert_eq!(evt.pool, Address::from_slice(&pool_id[12..32]));
        // Signed magnitudes; larger leg reported as amount_in
        assert_eq!(evt.amount_in, U256::from(5_000u64));
        assert_eq!(evt.amount_out, U256::from(2_500u64));
    }

    #[test]
    fn decode_infinity_cl_swap_uses_pool_id_topic() {
        // Pancake Infinity CL emits from the singleton CLPoolManager with the
        // same bytes32-PoolId scheme as V4, plus a trailing uint16 protocolFee
        // word (Swap data = 7 ABI words; decoder needs only the first two).
        let pool_id = b256!("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd");
        let mut amount0 = [0xffu8; 32];
        amount0[16..32].copy_from_slice(&(-12_000i128).to_be_bytes());
        let mut amount1 = [0u8; 32];
        amount1[16..32].copy_from_slice(&9_999i128.to_be_bytes());
        // sqrtPriceX96 + liquidity + tick + fee + protocolFee — zeroed, unused by the decoder.
        let tail = vec![0u8; 128 + 32];

        let log = make_log(
            Address::ZERO, // CLPoolManager singleton — not a pool address
            vec![*INF_CL_SWAP_TOPIC, pool_id],
            [amount0.to_vec(), amount1.to_vec(), tail].concat(),
        );

        let evt = decode_infinity_cl_swap(&log).unwrap();
        assert_eq!(evt.dex_type, "pancake_infinity");
        assert_eq!(evt.pool, Address::from_slice(&pool_id[12..32]));
        assert_eq!(evt.amount_in, U256::from(12_000u64));
        assert_eq!(evt.amount_out, U256::from(9_999u64));
    }

    #[test]
    fn decode_trader_joe_lb_packed_form() {
        let mut packed_in = [0u8; 32]; // X << 128 | Y
        packed_in[0..16].copy_from_slice(&700u128.to_be_bytes());
        let mut packed_out = [0u8; 32];
        packed_out[16..32].copy_from_slice(&300u128.to_be_bytes());
        let log = make_log(
            Address::ZERO,
            vec![*TRADER_JOE_LB_SWAP_LEGACY_TOPIC],
            [
                packed_in.to_vec(),
                packed_out.to_vec(),
                vec![0u8; 96], // volatility + totalFees + protocolFees
            ]
            .concat(),
        );
        let evt = decode_trader_joe_lb_swap(&log, Address::ZERO, true).unwrap();
        assert_eq!(evt.amount_in, U256::from(700u64));
        assert_eq!(evt.amount_out, U256::from(300u64));
    }

    #[test]
    fn decode_solidly_swap_four_amount_layout() {
        // Velodrome V2/Aerodrome: data = [amount0In, amount1In, amount0Out,
        // amount1Out]; one token1-sized input, token0-sized output.
        let a0_in = [0u8; 32];
        let mut a1_in = [0u8; 32];
        let mut a0_out = [0u8; 32];
        let a1_out = [0u8; 32];
        a1_in[24..32].copy_from_slice(&2_000_000u64.to_be_bytes());
        a0_out[24..32].copy_from_slice(&1_960_000u64.to_be_bytes());
        let log = make_log(
            Address::ZERO,
            vec![
                *SOLIDLY_SWAP_TOPIC,
                B256::ZERO, // sender
                B256::ZERO, // to
            ],
            [a0_in, a1_in, a0_out, a1_out].concat(),
        );
        let evt = decode_solidly_swap(&log, Address::ZERO).unwrap();
        assert_eq!(evt.dex_type, "solidly");
        assert_eq!(evt.amount_in, U256::from(2_000_000u64));
        assert_eq!(evt.amount_out, U256::from(1_960_000u64));

        // Short payload (< 128 bytes) must be skipped, not panic.
        let short = make_log(Address::ZERO, vec![*SOLIDLY_SWAP_TOPIC], vec![0u8; 96]);
        assert!(decode_solidly_swap(&short, Address::ZERO).is_none());
    }

    #[test]
    fn decode_transfer_from_log() {
        let from_addr = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let to_addr = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let value_bytes = {
            let mut buf = [0u8; 32];
            buf[24..32].copy_from_slice(&1000u64.to_be_bytes());
            buf
        };
        let topics_vec = vec![TRANSFER_TOPIC, from_addr, to_addr];
        let data_bytes = alloy::primitives::Bytes::from(value_bytes.to_vec());
        let data = LogData::new_unchecked(topics_vec, data_bytes);

        let log = Log {
            inner: alloy::primitives::Log { address: Address::ZERO, data },
            block_number: Some(100),
            block_hash: None,
            block_timestamp: None,
            transaction_hash: Some(b256!("0000000000000000000000000000000000000000000000000000000000000001")),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        };

        let decoded = decode_transfer(&log).unwrap();
        assert_eq!(decoded.from, Address::from_slice(&from_addr[12..]));
        assert_eq!(decoded.to, Address::from_slice(&to_addr[12..]));
        assert_eq!(decoded.value, U256::from(1000));
        assert_eq!(decoded.block, 100);
    }

    #[test]
    fn wrong_topic_returns_none() {
        let data = LogData::new_unchecked(vec![b256!("0000000000000000000000000000000000000000000000000000000000000001")], alloy::primitives::Bytes::new());

        let log = Log {
            inner: alloy::primitives::Log { address: Address::ZERO, data },
            block_number: Some(100),
            block_hash: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_transfer(&log).is_none());
        assert!(decode_aave_v3_flash(&log).is_none());
    }

    /// Regression: malformed logs with a short/empty data payload must be
    /// skipped, not panic (real-world Polygon providers returned such logs —
    /// found live by the cli_network_coverage E2E tests).
    #[test]
    fn short_data_payloads_are_skipped_not_panicking() {
        let transfer_like = |data_bytes: Vec<u8>| {
            make_log(
                Address::ZERO,
                vec![
                    TRANSFER_TOPIC,
                    b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                    b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                ],
                data_bytes,
            )
        };
        assert!(decode_transfer(&transfer_like(vec![])).is_none());
        assert!(decode_transfer(&transfer_like(vec![0u8; 31])).is_none());

        let v3_like = make_log(Address::ZERO, vec![V3_SWAP_TOPIC], vec![0u8; 16]);
        assert!(decode_uniswap_v3_swap(&v3_like, Address::ZERO).is_none());

        let v2_like = make_log(Address::ZERO, vec![V2_SWAP_TOPIC], vec![0u8; 32]);
        assert!(decode_uniswap_v2_swap(&v2_like, Address::ZERO).is_none());
    }
}
