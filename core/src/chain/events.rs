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

pub const V2_SYNC_TOPIC: B256 =
    b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

pub const V2_PAIR_CREATED_TOPIC: B256 =
    b256!("0d3648bd0f6ba80134a33ba9275ac585d9d315b0ad63f114424ee8c719964e50");

// ── Uniswap V3 ──────────────────────────────────────────────────────

pub const V3_SWAP_TOPIC: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

pub static V3_MINT_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)"));

pub const V3_BURN_TOPIC: B256 =
    b256!("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");

pub static V3_POOL_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("PoolCreated(address,address,uint24,int24,address)"));

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

pub static V4_INITIALIZE_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Initialize(bytes32 indexed id,Address indexed currency0,Address indexed currency1,uint24 fee,int24 tickSpacing,Address hooks)")
});

// ── Balancer V2 ─────────────────────────────────────────────────────

pub static BALANCER_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,bytes)"));

pub static BALANCER_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(bytes32,address,address,uint256,uint256)"));

pub static BALANCER_POOL_REGISTERED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("PoolRegistered(bytes32,address,uint8)"));

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

pub static SOLIDLY_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("PairCreated(address,address,bool,address)"));

pub static SOLIDLY_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(uint256,uint256,address,address)"));

// ── Camelot ─────────────────────────────────────────────────────────

pub static CAMELOT_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("PairCreated(address,address,address,uint256,bool)"));

// ── Curve ───────────────────────────────────────────────────────────

pub static CURVE_TOKEN_EXCHANGE_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256)"));

pub static CURVE_V2_TOKEN_EXCHANGE_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256,uint256)"));

pub static CURVE_POOL_ADDED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("PoolAdded(address,uint256)"));

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

pub static PENDLE_NEW_MARKET_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("NewMarket(address,address,uint256)"));

/// Pendle V2 market Swap event (caller, receiver indexed).
pub static PENDLE_MARKET_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,int256,int256,uint256,uint256)"));

pub static TRADER_JOE_LB_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("LBPairCreated(address,address,address,uint256,address[])"));

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
    let from = Address::from_slice(&topics[1][12..]);
    let to = Address::from_slice(&topics[2][12..]);
    let value = U256::from_be_slice(&log.data().data[0..32]);
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
pub fn decode_uniswap_v3_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    let amount_in_raw = U256::from_be_slice(&data[0..32]);
    let amount_out_raw = U256::from_be_slice(&data[32..64]);
    if amount_in_raw.is_zero() && amount_out_raw.is_zero() {
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
        amount_in: amount_in_raw,
        amount_out: amount_out_raw,
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
    let a0 = I256::from_be_slice(&data[0..32]).wrapping_abs();
    let a1 = I256::from_be_slice(&data[32..64]).wrapping_abs();
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

/// Decode a Uniswap V2 Swap event log.
pub fn decode_uniswap_v2_swap(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    let amount_in_raw = U256::from_be_slice(&data[0..32]);
    let amount_out_raw = U256::from_be_slice(&data[32..64]);
    Some(TradeEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        pool,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in: amount_in_raw,
        amount_out: amount_out_raw,
        dex_type: "uniswap_v2".to_string(),
    })
}

/// Decode a Curve TokenExchange event log.
pub fn decode_curve_exchange(log: &Log, pool: Address) -> Option<TradeEvent> {
    let data = &log.data().data;
    let amount_in = if data.len() >= 32 {
        U256::from_be_slice(&data[32..64])
    } else {
        return None;
    };
    let amount_out = if data.len() >= 96 {
        U256::from_be_slice(&data[64..96])
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
    let pt = I256::from_be_slice(&data[0..32]).wrapping_abs();
    let sy = I256::from_be_slice(&data[32..64]).wrapping_abs();
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
        let mut amount0 = [0u8; 32];
        amount0[16..32].copy_from_slice(&(-5_000i128).to_be_bytes()); // sign-extended int128
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
    fn decode_transfer_from_log() {
        let from_addr = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let to_addr = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let value_bytes = {
            let mut buf = [0u8; 32];
            buf[24..32].copy_from_slice(&1000u64.to_be_bytes());
            buf
        };
        let mut topics_vec = vec![TRANSFER_TOPIC, from_addr, to_addr];
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
}
