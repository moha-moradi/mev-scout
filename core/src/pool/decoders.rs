//! Event log decoders for Uniswap V2/V3, Curve, and Balancer pool interactions.

use std::sync::LazyLock;

use alloy::primitives::{b256, keccak256, Address, B256, I256, U256};

use crate::data::ExecutedLog;
use crate::utils::u128_from_be_bytes;

/// Uniswap V3: Swap(address sender, address recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick)
pub const V3_SWAP_TOPIC: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");
/// Uniswap V3: Mint(address sender, address owner, int24 tickLower, int24 tickUpper, uint128 amount, uint256 amount0, uint256 amount1)
///
/// Must stay equal to `keccak256` of that signature — a mistyped constant here
/// silently disables every V3 Mint decode, which makes JIT detection
/// structurally impossible without any other visible failure.
pub const V3_MINT_TOPIC: B256 =
    b256!("7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde");
/// Uniswap V3: Burn(address sender, address owner, int24 tickLower, int24 tickUpper, uint128 amount, uint256 amount0, uint256 amount1)
pub const V3_BURN_TOPIC: B256 =
    b256!("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");
/// Curve: TokenExchange(address buyer, int128 coin_sold, uint256 amount_sold, int128 coin_bought, uint256 amount_bought)
pub const CURVE_TOKEN_EXCHANGE_TOPIC: B256 =
    b256!("c55585ff3bfce0c9464a33a97ee0b031ca9555e103e1684770dca3311d769fe9");
/// Curve v2: TokenExchange(address buyer, int128 sold_id, uint256 tokens_sold, int128 bought_id, uint256 tokens_bought)
pub const CURVE_V2_TOKEN_EXCHANGE_TOPIC: B256 =
    b256!("9f586ef8f430130fa2ce9c4aeb45fa4c37f2d0e79abe7f4f039ad7fceaaea7c4");
/// Balancer V2: Swap(bytes32 indexed poolId, address indexed tokenIn, address indexed tokenOut, uint256 amountIn, uint256 amountOut)
pub const BALANCER_SWAP_TOPIC: B256 =
    b256!("fb412c811a17d1a8ad0ecab229fb91d821f5bbe210a5a6feae3dc626faf608d1");

/// Trader Joe LB 2.0/2.2: Swap(address indexed sender, address indexed to,
/// bool swapForY, uint256 amountIn, uint256 amountOutX, uint256 amountOutY,
/// uint256 totalFee, uint256 flashParameter).  Hash verified against the
/// canonical LB 2.0 signature `Swap(address,address,uint256,bool,uint256,
/// uint256,uint256,uint256)`.
pub static LB_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| *crate::chain::events::TRADER_JOE_LB_SWAP_TOPIC);

/// LFJ / Pharaoh LB: `DepositedToBins(address,address,uint256[],bytes32[])`.
pub static LB_DEPOSITED_TO_BINS_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"DepositedToBins(address,address,uint256[],bytes32[])"));

/// LFJ / Pharaoh LB: `WithdrawnFromBins(address,address,uint256[],bytes32[])`.
pub static LB_WITHDRAWN_FROM_BINS_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"WithdrawnFromBins(address,address,uint256[],bytes32[])"));

/// Pendle V2 Market Swap topic. Matches the canonical Pendle V2 signature
/// `Swap(address indexed caller, address indexed receiver, int256
/// netPtToAccount, int256 netSyToAccount, uint256 netSyFee, uint256
/// netSyToReserve)`.
pub static PENDLE_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| *crate::chain::events::PENDLE_MARKET_SWAP_TOPIC);

/// Velodrome V2/Aerodrome pool Swap topic (`Swap(address,address,uint256,
/// uint256,uint256,uint256)` — verified against velodrome-finance/contracts
/// `IPool.sol`). Data layout is the same four-amount word sequence as the
/// Uniswap V2 Swap event.
pub static SOLIDLY_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| *crate::chain::events::SOLIDLY_SWAP_TOPIC);

/// Fluid DEX pool: Swap(bool swap0to1, uint256 amountIn, uint256 amountOut, address to)
/// (verified against Instadapp/fluid-contracts-public poolT1/coreModule/events.sol).
/// No indexed params — everything is in the data words.
pub static FLUID_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"Swap(bool,uint256,uint256,address)"));

/// Metric V2 pool: Swap(address sender, address recipient, bool exactInput,
/// int128 amount0Delta, int128 amount1Delta, int16 newTick, uint104 newPositionInBin)
/// (swap-event signature — topic digest computed from the signature string;
/// verifying the produced topic on-chain is deferred).
pub static METRIC_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"Swap(address,address,bool,int128,int128,int16,uint104)"));

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
        amount0_bytes[16],
        amount0_bytes[17],
        amount0_bytes[18],
        amount0_bytes[19],
        amount0_bytes[20],
        amount0_bytes[21],
        amount0_bytes[22],
        amount0_bytes[23],
        amount0_bytes[24],
        amount0_bytes[25],
        amount0_bytes[26],
        amount0_bytes[27],
        amount0_bytes[28],
        amount0_bytes[29],
        amount0_bytes[30],
        amount0_bytes[31],
    ]);

    // amount1 is signed int256, bytes 32..64
    let amount1_bytes: [u8; 32] = log.data[32..64].try_into().ok()?;
    let amount1 = i128::from_be_bytes([
        amount1_bytes[16],
        amount1_bytes[17],
        amount1_bytes[18],
        amount1_bytes[19],
        amount1_bytes[20],
        amount1_bytes[21],
        amount1_bytes[22],
        amount1_bytes[23],
        amount1_bytes[24],
        amount1_bytes[25],
        amount1_bytes[26],
        amount1_bytes[27],
        amount1_bytes[28],
        amount1_bytes[29],
        amount1_bytes[30],
        amount1_bytes[31],
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
///
/// Matches `IUniswapV3PoolEvents` in Uniswap v3-core: `owner`, `tickLower` and
/// `tickUpper` are all `indexed`, so they live in the topics. The non-indexed
/// data is `[sender, amount, amount0, amount1]` for Mint (128 bytes) and
/// `[amount, amount0, amount1]` for Burn (96 bytes).
pub fn decode_v3_mint_burn(log: &ExecutedLog) -> Option<V3MintBurnDecoded> {
    if log.topics.len() < 4 {
        return None;
    }
    let is_burn = log.topics[0] == V3_BURN_TOPIC;
    if log.topics[0] != *V3_MINT_TOPIC && !is_burn {
        return None;
    }

    let tick_lower = i24_from_topic(&log.topics[2]);
    let tick_upper = i24_from_topic(&log.topics[3]);

    // Mint carries a leading `sender` word, Burn does not.
    let (need, amount_off) = if is_burn { (96, 0) } else { (128, 32) };
    if log.data.len() < need {
        return None;
    }
    let raw = u128_from_be_bytes(&log.data[amount_off..amount_off + 32]);
    let amount = if is_burn { -(raw as i128) } else { raw as i128 };

    Some(V3MintBurnDecoded {
        tick_lower,
        tick_upper,
        amount,
    })
}

/// Sign-extend a left-padded 24-bit tick (int24) from a 32-byte topic.
fn i24_from_topic(word: &B256) -> i32 {
    let raw = u32::from_be_bytes([word.0[28], word.0[29], word.0[30], word.0[31]]) & 0x00ff_ffff;
    if raw & 0x0080_0000 != 0 {
        (raw as i32) - 0x0100_0000
    } else {
        raw as i32
    }
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

/// Result of decoding a Trader Joe LB Swap event.
#[derive(Debug, Clone)]
pub struct LBSwapDecoded {
    /// Amount of the input token entering the pool.
    pub amount_in: u128,
    /// Amount of the output token leaving the pool.
    pub amount_out: u128,
    /// true = swapping token0 (X) for token1 (Y); false = Y for X.
    pub swap_for_y: bool,
}

/// Attempt to decode a Trader Joe LB 2.0/2.2 Swap event from an executed log.
///
/// LB 2.0 / 2.2 event (non-legacy): data layout is
/// `[swapForY(bool), amountIn(uint256), amountOutX(uint256),
/// amountOutY(uint256), totalFee(uint256), flashParameter(uint256)]`.
/// Indexed topics: `[sig, sender, to]`.
pub fn decode_lb_swap(log: &ExecutedLog) -> Option<LBSwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != *LB_SWAP_TOPIC {
        return None;
    }
    if log.data.len() < 96 {
        return None;
    }
    let swap_for_y = log.data[31] != 0;
    let amount_in = u128_from_be_bytes(&log.data[32..64]);
    let amount_out = u128_from_be_bytes(&log.data[64..96]);

    Some(LBSwapDecoded {
        amount_in,
        amount_out,
        swap_for_y,
    })
}

/// Result of decoding a Pendle V2 Market Swap event.
#[derive(Debug, Clone)]
pub struct PendleSwapDecoded {
    /// Whether the swap results in net PT outflow from the AMM
    /// (netPtToAccount > 0).
    pub is_net_pt_out: bool,
    /// Amount of the input token going into the AMM.
    pub amount_in: u128,
    /// Amount of the output token coming out of the AMM.
    pub amount_out: u128,
}

/// Attempt to decode a Pendle V2 Market Swap event from an executed log.
///
/// V2 event: `Swap(address indexed caller, address indexed receiver, int256
/// netPtToAccount, int256 netSyToAccount, uint256 netSyFee, uint256
/// netSyToReserve)`. Topics: [sig, caller, receiver]. Data: 4 ABI words.
///
/// When netPtToAccount > 0 the user is buying PT (SY in, PT out);
/// when < 0 the user is selling PT (PT in, SY out).
pub fn decode_pendle_swap(log: &ExecutedLog) -> Option<PendleSwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != *PENDLE_SWAP_TOPIC {
        return None;
    }
    if log.topics.len() < 3 || log.data.len() < 64 {
        return None;
    }
    let net_pt = I256::try_from_be_slice(&log.data[0..32]).unwrap_or(I256::ZERO);
    let net_sy = I256::try_from_be_slice(&log.data[32..64]).unwrap_or(I256::ZERO);
    let is_net_pt_out = net_pt.is_positive();
    let (amount_in, amount_out) = if is_net_pt_out {
        (
            net_sy.wrapping_abs().into_raw().to::<u128>(),
            net_pt.into_raw().to::<u128>(),
        )
    } else {
        (
            net_pt.wrapping_abs().into_raw().to::<u128>(),
            net_sy.into_raw().to::<u128>(),
        )
    };
    Some(PendleSwapDecoded {
        is_net_pt_out,
        amount_in,
        amount_out,
    })
}

/// Result of decoding a Fluid DEX pool Swap event.
#[derive(Debug, Clone)]
pub struct FluidSwapDecoded {
    /// true = token0 → token1 direction.
    pub swap0to1: bool,
    pub amount_in: u128,
    pub amount_out: u128,
}

/// Attempt to decode a Fluid DEX pool Swap event from an executed log.
///
/// Event (verified against fluid-contracts-public): `Swap(bool swap0to1,
/// uint256 amountIn, uint256 amountOut, address to)` — no indexed params, so
/// data = [swap0to1 (32B), amountIn (32B), amountOut (32B), to (32B)].
pub fn decode_fluid_swap(log: &ExecutedLog) -> Option<FluidSwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != *FLUID_SWAP_TOPIC {
        return None;
    }
    if log.data.len() < 96 {
        return None;
    }
    let swap0to1 = log.data[31] != 0;
    let amount_in = u128_from_be_bytes(&log.data[32..64]);
    let amount_out = u128_from_be_bytes(&log.data[64..96]);
    Some(FluidSwapDecoded {
        swap0to1,
        amount_in,
        amount_out,
    })
}

/// Result of decoding a Metric V2 Swap event.
#[derive(Debug, Clone)]
pub struct MetricSwapDecoded {
    pub exact_input: bool,
    pub amount0_delta: i128,
    pub amount1_delta: i128,
    pub new_tick: i16,
    pub new_position_in_bin: u128,
}

/// Attempt to decode a Metric V2 Swap event from an executed log.
///
/// Event: `Swap(address sender, address recipient, bool exactInput,
/// int128 amount0Delta, int128 amount1Delta, int16 newTick, uint104
/// newPositionInBin)` — sender/recipient assumed indexed (topics[1..2]); data
/// = [exactInput (32B), amount0Delta (32B), amount1Delta (32B), newTick (32B),
/// newPositionInBin (32B, uint104 right-aligned)] = 160 bytes.
pub fn decode_metric_swap(log: &ExecutedLog) -> Option<MetricSwapDecoded> {
    if log.topics.is_empty() || log.topics[0] != *METRIC_SWAP_TOPIC {
        return None;
    }
    if log.data.len() < 160 {
        return None;
    }
    let exact_input = log.data[31] != 0;
    // int128 values are ABI-encoded as 32-byte words; the value sits in the
    // lower 16 bytes (right-aligned, sign-extended into the upper 16).
    let amount0_delta = i128::from_be_bytes(log.data[48..64].try_into().ok()?);
    let amount1_delta = i128::from_be_bytes(log.data[80..96].try_into().ok()?);
    // newTick is int16 sign-extended into its 32-byte word (rightmost 2 bytes).
    let new_tick = i16::from_be_bytes(log.data[126..128].try_into().ok()?);
    // newPositionInBin is uint104 in the 5th ABI word (data[128..160]).
    let new_position_in_bin = u128_from_be_bytes(&log.data[128..160]);
    Some(MetricSwapDecoded {
        exact_input,
        amount0_delta,
        amount1_delta,
        new_tick,
        new_position_in_bin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::keccak256;

    /// The Uniswap V3 topics are hardcoded `b256!` literals, so nothing else
    /// checks them. A mistyped hash fails silently: the decoder just never
    /// matches, and the only symptom is a kind that can never be detected (this
    /// is exactly how `V3_MINT_TOPIC` was wrong, which made JIT undetectable).
    #[test]
    fn uniswap_v3_topics_match_keccak_of_canonical_signatures() {
        for (topic, sig) in [
            (
                V3_SWAP_TOPIC,
                "Swap(address,address,int256,int256,uint160,uint128,int24)",
            ),
            (
                V3_MINT_TOPIC,
                "Mint(address,address,int24,int24,uint128,uint256,uint256)",
            ),
            (
                V3_BURN_TOPIC,
                "Burn(address,int24,int24,uint128,uint256,uint256)",
            ),
        ] {
            assert_eq!(
                topic,
                keccak256(sig),
                "topic drifted from keccak256(\"{sig}\")"
            );
        }
    }

    /// A real V3 Mint log decodes — the end-to-end consequence of the topic
    /// being right. Fixture captured from mainnet block 26051637 on the
    /// UniswapV3 USDC/WETH 0.05% pool.
    #[test]
    fn v3_mint_topic_decodes_a_mint() {
        // topics: [sig, owner, tickLower, tickUpper]
        let log = ExecutedLog {
            address: alloy::primitives::address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
            topics: vec![
                V3_MINT_TOPIC,
                alloy::primitives::b256!(
                    "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"
                ),
                alloy::primitives::b256!(
                    "0000000000000000000000000000000000000000000000000000000000030246"
                ),
                alloy::primitives::b256!(
                    "00000000000000000000000000000000000000000000000000000000000303f4"
                ),
            ],
            // data: sender, amount, amount0, amount1
            data: alloy::primitives::hex::decode(concat!(
                "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88",
                "0000000000000000000000000000000000000000000000000004ca38c9eecc93",
                "000000000000000000000000000000000000000000000000000000002d5b38bf",
                "00000000000000000000000000000000000000000000000003c9c2b775e90ef4",
            ))
            .unwrap()
            .into(),
        };
        let decoded = decode_v3_mint_burn(&log).expect("Mint log must decode");
        assert_eq!(decoded.tick_lower, 197190);
        assert_eq!(decoded.tick_upper, 197620);
        assert_eq!(
            decoded.amount, 0x4ca38c9eecc93_i128,
            "a Mint amount is positive"
        );
    }

    /// A real V3 Burn log decodes, and `amount` keeps its negative sign so the
    /// liquidity delta adds up. Same block/pool as the Mint above.
    #[test]
    fn v3_burn_topic_decodes_a_burn() {
        let log = ExecutedLog {
            address: alloy::primitives::address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
            topics: vec![
                V3_BURN_TOPIC,
                alloy::primitives::b256!(
                    "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"
                ),
                alloy::primitives::b256!(
                    "000000000000000000000000000000000000000000000000000000000003011a"
                ),
                alloy::primitives::b256!(
                    "00000000000000000000000000000000000000000000000000000000000302b4"
                ),
            ],
            // data: amount, amount0, amount1
            data: alloy::primitives::hex::decode(concat!(
                "000000000000000000000000000000000000000000000000000503364091d046",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "00000000000000000000000000000000000000000000000007a45af71b4f9a78",
            ))
            .unwrap()
            .into(),
        };
        let decoded = decode_v3_mint_burn(&log).expect("Burn log must decode");
        assert_eq!(decoded.tick_lower, 196890);
        assert_eq!(decoded.tick_upper, 197300);
        assert_eq!(
            decoded.amount,
            -(0x0503364091d046i128),
            "a Burn amount is negative"
        );
    }

    /// A Mint carries a leading `sender` word that a Burn does not, so the
    /// amount sits in a different data word for each.
    #[test]
    fn v3_mint_and_burn_amount_offsets_differ() {
        let topics = |t0: B256| {
            vec![
                t0,
                alloy::primitives::b256!(
                    "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"
                ),
                alloy::primitives::b256!(
                    "0000000000000000000000000000000000000000000000000000000000030246"
                ),
                alloy::primitives::b256!(
                    "00000000000000000000000000000000000000000000000000000000000303f4"
                ),
            ]
        };
        let pool = alloy::primitives::address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");

        // Same numeric amount, placed in word 0 (Burn) and word 1 (Mint).
        let burn = ExecutedLog {
            address: pool,
            topics: topics(V3_BURN_TOPIC),
            data: {
                let mut d = vec![0u8; 96];
                d[16..32].copy_from_slice(&0x1234u128.to_be_bytes());
                d.into()
            },
        };
        let mint = ExecutedLog {
            address: pool,
            topics: topics(V3_MINT_TOPIC),
            data: {
                let mut d = vec![0u8; 128];
                d[48..64].copy_from_slice(&0x1234u128.to_be_bytes());
                d.into()
            },
        };
        assert_eq!(decode_v3_mint_burn(&burn).unwrap().amount, -(0x1234i128));
        assert_eq!(decode_v3_mint_burn(&mint).unwrap().amount, 0x1234i128);
    }
}
