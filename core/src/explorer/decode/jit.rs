//! V3 Mint/Burn and LB bin deposit/withdraw JIT facts.
use alloy::primitives::{B256, U256};
use crate::utils::topic_address;

use crate::data::LogData;
use crate::explorer::types::JitFact;
use crate::pool::decoders::{
    LB_DEPOSITED_TO_BINS_TOPIC, LB_WITHDRAWN_FROM_BINS_TOPIC, V3_BURN_TOPIC, V3_MINT_TOPIC,
};

/// Sign-extend a left-padded 24-bit tick value (int24) from a 32-byte topic.
fn i24_from_topic(word: &B256) -> i32 {
    let raw = u32::from_be_bytes([word.0[28], word.0[29], word.0[30], word.0[31]]) & 0x00ff_ffff;
    if raw & 0x0080_0000 != 0 {
        (raw as i32) - 0x0100_0000
    } else {
        raw as i32
    }
}

/// Decode a V3 Mint/Burn fact (JIT candidate).
///
/// Layout taken from `IUniswapV3PoolEvents` in Uniswap/v3-core, where `owner`,
/// `tickLower` and `tickUpper` are all declared `indexed`:
///
/// ```solidity
/// event Mint(address sender, address indexed owner, int24 indexed tickLower,
///            int24 indexed tickUpper, uint128 amount, uint256 amount0, uint256 amount1);
/// event Burn(address indexed owner, int24 indexed tickLower, int24 indexed tickUpper,
///            uint128 amount, uint256 amount0, uint256 amount1);
/// ```
///
/// So the topics are `[sig, owner, tickLower, tickUpper]` and the non-indexed
/// data is `[sender, amount, amount0, amount1]` for Mint (128 bytes) and
/// `[amount, amount0, amount1]` for Burn (96 bytes). Anything that does not match
/// that shape is rejected rather than guessed at, because a wrong offset
/// silently yields plausible-looking garbage ticks.
pub fn decode_v3_mint_burn(log: &LogData) -> Option<JitFact> {
    let topic0 = *log.topics.first()?;
    let is_mint = topic0 == V3_MINT_TOPIC;
    let is_burn = topic0 == V3_BURN_TOPIC;
    if !is_mint && !is_burn {
        return None;
    }
    if log.topics.len() < 4 {
        return None;
    }
    let owner = topic_address(log.topics[1]);
    let tick_lower = i24_from_topic(&log.topics[2]);
    let tick_upper = i24_from_topic(&log.topics[3]);

    // Mint carries a leading `sender` word, Burn does not.
    let (amount_off, (a0_off, a1_off)) = if is_mint {
        (32, (64, 96))
    } else {
        (0, (32, 64))
    };
    let need = a1_off + 32;
    if log.data.len() < need {
        return None;
    }
    let liquidity = crate::utils::u128_from_be_bytes(&log.data[amount_off + 16..amount_off + 32]);
    let amount0 = U256::from_be_slice(&log.data[a0_off..a0_off + 32]);
    let amount1 = U256::from_be_slice(&log.data[a1_off..a1_off + 32]);

    Some(JitFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        owner,
        tick_lower,
        tick_upper,
        is_mint,
        liquidity,
        amount0,
        amount1,
        bin_amm: false,
    })
}

/// Decode LFJ / Pharaoh LB `DepositedToBins` / `WithdrawnFromBins` into a
/// JIT fact. Bin ids are mapped into `tick_lower`/`tick_upper` as the inclusive
/// min/max id so existing JIT pairing can match deposit↔withdraw ranges.
pub fn decode_lb_bins_liquidity(log: &LogData) -> Option<JitFact> {
    let topic0 = *log.topics.first()?;
    let is_mint = topic0 == *LB_DEPOSITED_TO_BINS_TOPIC;
    let is_burn = topic0 == *LB_WITHDRAWN_FROM_BINS_TOPIC;
    if !is_mint && !is_burn {
        return None;
    }
    // topics: [sig, sender, to]; data = ABI(uint256[] ids, bytes32[] amounts)
    if log.topics.len() < 3 || log.data.len() < 64 {
        return None;
    }
    let owner = topic_address(log.topics[2]);
    let ids = decode_abi_u256_array(&log.data, 0)?;
    if ids.is_empty() {
        return None;
    }
    let mut min_id = i32::MAX;
    let mut max_id = i32::MIN;
    for id in &ids {
        let v = u256_to_i32_bin(*id)?;
        min_id = min_id.min(v);
        max_id = max_id.max(v);
    }
    if min_id == i32::MAX {
        return None;
    }

    let (amount0, amount1) = decode_lb_amounts_xy(&log.data).unwrap_or((U256::ZERO, U256::ZERO));
    Some(JitFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        owner,
        tick_lower: min_id,
        tick_upper: max_id,
        is_mint,
        liquidity: ids.len() as u128,
        amount0,
        amount1,
        bin_amm: true,
    })
}

fn u256_to_i32_bin(v: U256) -> Option<i32> {
    let n: u64 = v.try_into().ok()?;
    i32::try_from(n).ok()
}

fn u256_to_usize(v: U256) -> Option<usize> {
    let n: u64 = v.try_into().ok()?;
    usize::try_from(n).ok()
}

/// Read a dynamic `uint256[]` from ABI-encoded `data` starting at the head
/// word index `head_word` (0 = first 32-byte word is the relative offset).
fn decode_abi_u256_array(data: &[u8], head_word: usize) -> Option<Vec<U256>> {
    let head_off = head_word * 32;
    if data.len() < head_off + 32 {
        return None;
    }
    let rel = u256_to_usize(U256::from_be_slice(&data[head_off..head_off + 32]))?;
    if data.len() < rel + 32 {
        return None;
    }
    let len = u256_to_usize(U256::from_be_slice(&data[rel..rel + 32]))?;
    let mut out = Vec::with_capacity(len.min(64));
    for i in 0..len.min(64) {
        let start = rel + 32 + i * 32;
        if data.len() < start + 32 {
            break;
        }
        out.push(U256::from_be_slice(&data[start..start + 32]));
    }
    Some(out)
}

/// Sum packed X/Y amounts from the second dynamic `bytes32[]` in a
/// DepositedToBins / WithdrawnFromBins payload. Each bytes32 packs
/// amountX in the low 128 bits and amountY in the high 128 bits (LFJ layout).
fn decode_lb_amounts_xy(data: &[u8]) -> Option<(U256, U256)> {
    if data.len() < 64 {
        return None;
    }
    let rel = u256_to_usize(U256::from_be_slice(&data[32..64]))?;
    if data.len() < rel + 32 {
        return None;
    }
    let len = u256_to_usize(U256::from_be_slice(&data[rel..rel + 32]))?;
    let mut ax = U256::ZERO;
    let mut ay = U256::ZERO;
    for i in 0..len.min(64) {
        let start = rel + 32 + i * 32;
        if data.len() < start + 32 {
            break;
        }
        let word = &data[start..start + 32];
        // amountX = low 16 bytes, amountY = high 16 bytes (LFJ PackedUint128).
        ax = ax.saturating_add(U256::from_be_slice(&word[16..32]));
        ay = ay.saturating_add(U256::from_be_slice(&word[0..16]));
    }
    Some((ax, ay))
}
