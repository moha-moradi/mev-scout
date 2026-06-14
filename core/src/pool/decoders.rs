//! Event log decoders for Uniswap V2/V3, Curve, and Balancer pool interactions.

use alloy::primitives::{b256, keccak256, Address, B256, U256};

use crate::data::ExecutedLog;
use crate::utils::u128_from_be_bytes;

/// Uniswap V3: Swap(address sender, address recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick)
pub const V3_SWAP_TOPIC: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");
/// Uniswap V3: Mint(address sender, address owner, int24 tickLower, int24 tickUpper, uint128 amount, uint256 amount0, uint256 amount1)
pub static V3_MINT_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)"));
/// Uniswap V3: Burn(address sender, address owner, int24 tickLower, int24 tickUpper, uint128 amount, uint256 amount0, uint256 amount1)
pub const V3_BURN_TOPIC: B256 =
    b256!("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");
/// Curve: TokenExchange(address buyer, int128 coin_sold, uint256 amount_sold, int128 coin_bought, uint256 amount_bought)
pub static CURVE_TOKEN_EXCHANGE_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256)"));
/// Curve v2: TokenExchange(address buyer, int128 sold_id, uint256 tokens_sold, int128 bought_id, uint256 tokens_bought)
pub static CURVE_V2_TOKEN_EXCHANGE_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("TokenExchange(address,int128,uint256,int128,uint256,uint256)"));
/// Balancer V2: Swap(bytes32 indexed poolId, address indexed tokenIn, address indexed tokenOut, uint256 amountIn, uint256 amountOut)
pub static BALANCER_SWAP_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Swap(bytes32,address,address,uint256,uint256)"));

/// Result of decoding a V3 Swap event.
#[derive(Debug, Clone)]
pub struct V3SwapDecoded {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub amount0: i128,
    pub amount1: i128,
}

/// Result of decoding a V3 Mint/Burn event.
#[derive(Debug, Clone)]
pub struct V3MintBurnDecoded {
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub amount: i128,
}

/// Result of decoding a Curve TokenExchange event.
#[derive(Debug, Clone)]
pub struct CurveSwapDecoded {
    pub coin_sold: u128,
    pub amount_sold: u128,
    pub coin_bought: u128,
    pub amount_bought: u128,
}

/// Result of decoding a Balancer Swap event.
#[derive(Debug, Clone)]
pub struct BalancerSwapDecoded {
    pub pool_id: [u8; 32],
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: u128,
    pub amount_out: u128,
}

/// Attempt to decode a V3 Swap event from an executed log.
pub fn decode_v3_swap(log: &ExecutedLog) -> Option<V3SwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != V3_SWAP_TOPIC {
        return None;
    }
    // topics: sender, recipient
    // data: int256 amount0 (32), int256 amount1 (32), uint160 sqrtPriceX96 (32),
    //       uint128 liquidity (32), int24 tick (32)
    if log.data.len() < 160 {
        return None;
    }

    // amount0 is signed int256, bytes 0..32
    let amount0_bytes: [u8; 32] = log.data[..32].try_into().ok()?;
    let amount0 = i128::from_be_bytes([
        amount0_bytes[16], amount0_bytes[17], amount0_bytes[18], amount0_bytes[19],
        amount0_bytes[20], amount0_bytes[21], amount0_bytes[22], amount0_bytes[23],
        amount0_bytes[24], amount0_bytes[25], amount0_bytes[26], amount0_bytes[27],
        amount0_bytes[28], amount0_bytes[29], amount0_bytes[30], amount0_bytes[31],
    ]);

    // amount1 is signed int256, bytes 32..64
    let amount1_bytes: [u8; 32] = log.data[32..64].try_into().ok()?;
    let amount1 = i128::from_be_bytes([
        amount1_bytes[16], amount1_bytes[17], amount1_bytes[18], amount1_bytes[19],
        amount1_bytes[20], amount1_bytes[21], amount1_bytes[22], amount1_bytes[23],
        amount1_bytes[24], amount1_bytes[25], amount1_bytes[26], amount1_bytes[27],
        amount1_bytes[28], amount1_bytes[29], amount1_bytes[30], amount1_bytes[31],
    ]);

    let sqrt_price_x96 = U256::from_be_slice(&log.data[64..96]);
    let liquidity = u128_from_be_bytes(&log.data[96..128]);

    // tick is int24, stored right-aligned in 32 bytes
    let tick_bytes: [u8; 32] = log.data[128..160].try_into().ok()?;
    let tick = i32::from_be_bytes([
        tick_bytes[28],
        tick_bytes[29],
        tick_bytes[30],
        tick_bytes[31],
    ]);

    Some(V3SwapDecoded {
        sqrt_price_x96,
        tick,
        liquidity,
        amount0,
        amount1,
    })
}

/// Attempt to decode a V3 Mint or Burn event from an executed log.
pub fn decode_v3_mint_burn(log: &ExecutedLog) -> Option<V3MintBurnDecoded> {
    if log.topics.is_empty() {
        return None;
    }
    let is_mint_or_burn = log.topics[0] == *V3_MINT_TOPIC || log.topics[0] == V3_BURN_TOPIC;
    if !is_mint_or_burn {
        return None;
    }
    // topics: sender, owner
    // data: int24 tickLower (32), int24 tickUpper (32), uint128 amount (32), ...
    if log.data.len() < 96 {
        return None;
    }

    let lower_bytes: [u8; 32] = log.data[..32].try_into().ok()?;
    let tick_lower = i32::from_be_bytes([
        lower_bytes[28],
        lower_bytes[29],
        lower_bytes[30],
        lower_bytes[31],
    ]);
    let upper_bytes: [u8; 32] = log.data[32..64].try_into().ok()?;
    let tick_upper = i32::from_be_bytes([
        upper_bytes[28],
        upper_bytes[29],
        upper_bytes[30],
        upper_bytes[31],
    ]);
    let raw = u128_from_be_bytes(&log.data[64..96]);
    let amount = if log.topics[0] == V3_BURN_TOPIC {
        -(raw as i128)
    } else {
        raw as i128
    };

    Some(V3MintBurnDecoded {
        tick_lower,
        tick_upper,
        amount,
    })
}

/// Attempt to decode a Curve TokenExchange event.
pub fn decode_curve_swap(log: &ExecutedLog) -> Option<CurveSwapDecoded> {
    if log.topics.is_empty() {
        return None;
    }
    let is_curve = log.topics[0] == *CURVE_TOKEN_EXCHANGE_TOPIC
        || log.topics[0] == *CURVE_V2_TOKEN_EXCHANGE_TOPIC;
    if !is_curve {
        return None;
    }
    // topics[1]: buyer
    // data: int128 coin_sold (32), uint256 amount_sold (32), int128 coin_bought (32), uint256 amount_bought (32)
    if log.data.len() < 128 {
        return None;
    }
    let coin_sold = u128_from_be_bytes(&log.data[..32]);
    let amount_sold = u128_from_be_bytes(&log.data[32..64]);
    let coin_bought = u128_from_be_bytes(&log.data[64..96]);
    let amount_bought = u128_from_be_bytes(&log.data[96..128]);

    Some(CurveSwapDecoded {
        coin_sold,
        amount_sold,
        coin_bought,
        amount_bought,
    })
}

/// Attempt to decode a Balancer V2 Swap event.
pub fn decode_balancer_swap(log: &ExecutedLog) -> Option<BalancerSwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != *BALANCER_SWAP_TOPIC {
        return None;
    }
    // topics: topic[0]=sig, topic[1]=poolId (bytes32), topic[2]=tokenIn, topic[3]=tokenOut
    // data: uint256 amountIn, uint256 amountOut
    if log.topics.len() < 4 {
        return None;
    }
    if log.data.len() < 64 {
        return None;
    }

    let pool_id: [u8; 32] = log.topics[1].into();
    let token_in = Address::from_slice(&log.topics[2].as_slice()[12..]);
    let token_out = Address::from_slice(&log.topics[3].as_slice()[12..]);
    let amount_in = u128_from_be_bytes(&log.data[..32]);
    let amount_out = u128_from_be_bytes(&log.data[32..64]);

    Some(BalancerSwapDecoded {
        pool_id,
        token_in,
        token_out,
        amount_in,
        amount_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_v3_swap_log(sqrt: U256, liq: u128, t: i32) -> ExecutedLog {
        make_v3_swap_log_with_amounts(sqrt, liq, t, 0, 0)
    }

    fn make_v3_swap_log_with_amounts(sqrt: U256, liq: u128, t: i32, amt0: i128, amt1: i128) -> ExecutedLog {
        let mut data = Vec::with_capacity(160);
        let amt0_be = amt0.to_be_bytes();
        let mut b = [0u8; 32];
        b[16..32].copy_from_slice(&amt0_be);
        data.extend_from_slice(&b);
        let amt1_be = amt1.to_be_bytes();
        b = [0u8; 32];
        b[16..32].copy_from_slice(&amt1_be);
        data.extend_from_slice(&b);
        b = [0u8; 32];
        b.copy_from_slice(&sqrt.to_be_bytes::<32>());
        data.extend_from_slice(&b);

        let liq_be = liq.to_be_bytes();
        b = [0u8; 32];
        b[16..32].copy_from_slice(&liq_be);
        data.extend_from_slice(&b);

        let tick_be = t.to_be_bytes();
        b = [0u8; 32];
        b[28..32].copy_from_slice(&tick_be);
        data.extend_from_slice(&b);

        ExecutedLog {
            address: Address::ZERO,
            topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    #[test]
    fn test_decode_v3_swap_basic() {
        let sqrt = U256::from(2u128.pow(96)); // 1.0 price
        let liq = 1_000_000_000u128;
        let tick = 0i32;
        let log = make_v3_swap_log(sqrt, liq, tick);
        let decoded = decode_v3_swap(&log).unwrap();
        assert_eq!(decoded.sqrt_price_x96, sqrt);
        assert_eq!(decoded.liquidity, liq);
        assert_eq!(decoded.tick, tick);
        assert_eq!(decoded.amount0, 0);
        assert_eq!(decoded.amount1, 0);
    }

    #[test]
    fn test_decode_v3_swap_with_amounts() {
        let sqrt = U256::from(2u128.pow(96));
        let liq = 1_000_000_000u128;
        let tick = 10i32;
        let log = make_v3_swap_log_with_amounts(sqrt, liq, tick, 1000, -990);
        let decoded = decode_v3_swap(&log).unwrap();
        assert_eq!(decoded.amount0, 1000);
        assert_eq!(decoded.amount1, -990);
    }

    #[test]
    fn test_decode_v3_swap_wrong_topic() {
        let log = ExecutedLog {
            address: Address::ZERO,
            topics: vec![B256::ZERO],
            data: Default::default(),
        };
        assert!(decode_v3_swap(&log).is_none());
    }

    #[test]
    fn test_decode_v3_mint_burn() {
        let mut data = Vec::new();
        let mut padded = [0u8; 32];
        let lower = -88720i32;
        let lower_be = lower.to_be_bytes();
        padded[28..32].copy_from_slice(&lower_be);
        data.extend_from_slice(&padded);

        let upper = 88720i32;
        let upper_be = upper.to_be_bytes();
        padded[28..32].copy_from_slice(&upper_be);
        data.extend_from_slice(&padded);

        let amount = 500_000u128;
        let amount_be = amount.to_be_bytes();
        padded = [0u8; 32];
        padded[16..32].copy_from_slice(&amount_be);
        data.extend_from_slice(&padded);

        let log = ExecutedLog {
            address: Address::ZERO,
            topics: vec![*V3_MINT_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        };
        let decoded = decode_v3_mint_burn(&log).unwrap();
        assert_eq!(decoded.tick_lower, -88720);
        assert_eq!(decoded.tick_upper, 88720);
        assert_eq!(decoded.amount, 500_000);
    }

    #[test]
    fn test_decode_curve_swap() {
        let mut data = Vec::new();
        // coin_sold = 0
        let mut padded = [0u8; 32];
        data.extend_from_slice(&padded);
        // amount_sold = 1000
        let amt = 1000u128;
        let amt_be = amt.to_be_bytes();
        padded[16..32].copy_from_slice(&amt_be);
        data.extend_from_slice(&padded);
        // coin_bought = 1
        padded = [0u8; 32];
        padded[31] = 1;
        data.extend_from_slice(&padded);
        // amount_bought = 999
        let amt2 = 999u128;
        let amt2_be = amt2.to_be_bytes();
        padded = [0u8; 32];
        padded[16..32].copy_from_slice(&amt2_be);
        data.extend_from_slice(&padded);

        let log = ExecutedLog {
            address: Address::ZERO,
            topics: vec![*CURVE_TOKEN_EXCHANGE_TOPIC, B256::ZERO],
            data: data.into(),
        };
        let decoded = decode_curve_swap(&log).unwrap();
        assert_eq!(decoded.coin_sold, 0);
        assert_eq!(decoded.amount_sold, 1000);
        assert_eq!(decoded.coin_bought, 1);
        assert_eq!(decoded.amount_bought, 999);
    }

    #[test]
    fn test_decode_balancer_swap() {
        let pool_id = [0x42u8; 32];
        let token_in = Address::repeat_byte(0xaa);
        let token_out = Address::repeat_byte(0xbb);

        let mut data = Vec::new();
        let mut padded = [0u8; 32];
        let amt_in = 5000u128;
        let amt_in_be = amt_in.to_be_bytes();
        padded[16..32].copy_from_slice(&amt_in_be);
        data.extend_from_slice(&padded);

        let amt_out = 4990u128;
        let amt_out_be = amt_out.to_be_bytes();
        padded = [0u8; 32];
        padded[16..32].copy_from_slice(&amt_out_be);
        data.extend_from_slice(&padded);

        let addr_to_topic = |a: Address| -> B256 {
            let mut b = [0u8; 32];
            b[12..32].copy_from_slice(a.as_slice());
            B256::from(b)
        };
        let log = ExecutedLog {
            address: Address::ZERO,
            topics: vec![
                *BALANCER_SWAP_TOPIC,
                B256::from(pool_id),
                addr_to_topic(token_in),
                addr_to_topic(token_out),
            ],
            data: data.into(),
        };
        let decoded = decode_balancer_swap(&log).unwrap();
        assert_eq!(decoded.pool_id, pool_id);
        assert_eq!(decoded.token_in, token_in);
        assert_eq!(decoded.token_out, token_out);
        assert_eq!(decoded.amount_in, 5000);
        assert_eq!(decoded.amount_out, 4990);
    }
}
