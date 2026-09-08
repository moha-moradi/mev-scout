//! Receipt-log decoders for the explorer's forensic layer.
//!
//! Works directly on `LogData` entries from `ReceiptData` (the bulk-receipt
//! path) rather than `ExecutedLog` (the replay path), so the explorer never
//! needs the EVM replayer. Covers:
//! - raw ERC-20 `Transfer` (the accounting primitive for profit attribution)
//! - DEX swap events: V2, V3, V4, Curve, Balancer, Solidly, Trader Joe LB, Pendle
//! - liquidation registry: Aave V3 `LiquidationCall`, Compound V3 `Absorb`
//! - V3 `Mint`/`Burn` (JIT positions)
//!
//! Swap token direction is resolved by pairing each swap log with the ERC-20
//! Transfer legs that move tokens into/out of the pool in the same tx
//! (registry-free, chain-generic). Pool-registry lookups can enrich later but
//! are not required for classification.

use alloy::primitives::{Address, B256, U256};
use std::sync::LazyLock;

use crate::data::LogData;
use crate::explorer::types::{Amm, JitFact, LiquidationFact, SwapFact, TransferFact};

use crate::chain::events::{
    AAVE_V3_LIQUIDATION_CALL_TOPIC, COMPOUND_V3_ABSORB_TOPIC, TRANSFER_TOPIC, V2_SWAP_TOPIC,
    V3_SWAP_TOPIC, V4_SWAP_TOPIC,
};
use crate::pool::decoders::{
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    LB_SWAP_TOPIC, PENDLE_SWAP_TOPIC, V3_BURN_TOPIC, V3_MINT_TOPIC,
};

/// Solidly/Velodrome/Aerodrome Swap topic (`Swap(uint256,uint256,address,address)`),
/// mirrored here from `chain::events` for local reference.
pub static SOLIDLY_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    alloy::primitives::keccak256("Swap(uint256,uint256,address,address)")
});

/// Decode an ERC-20 Transfer fact from a receipt log.
pub fn decode_transfer(log: &LogData) -> Option<TransferFact> {
    if log.topics.len() < 3 || log.topics[0] != TRANSFER_TOPIC {
        return None;
    }
    if log.data.len() < 32 {
        return None;
    }
    Some(TransferFact {
        tx_index: 0, // stamped by caller
        log_index: 0, // stamped by caller
        token: log.address,
        from: Address::from_slice(&log.topics[1][12..]),
        to: Address::from_slice(&log.topics[2][12..]),
        amount: U256::from_be_slice(&log.data[0..32]),
    })
}

/// Decode a swap fact from a receipt log. Amount semantics per AMM family:
/// - V2: (amount0In, amount1In, amount0Out, amount1Out) — direction needs
///   token metadata, so the caller resolves direction via transfer pairing.
/// - V3: signed (amount0, amount1).
/// - V4: poolId topic + signed amounts (synthetic pool address = last 20 bytes).
/// - Curve/Balancer/LB/Pendle: explicit in/out amounts.
///
/// Returns the fact with zero tokens when direction is not resolvable from
/// the log alone; `attach_swap_tokens` fills them from transfer pairing.
pub fn decode_swap(log: &LogData) -> Option<(Amm, SwapFact)> {
    let topic0 = *log.topics.first()?;
    let base = |amm: Amm, pool: Address| SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in: U256::ZERO,
        amount_out: U256::ZERO,
    };

    if topic0 == V2_SWAP_TOPIC {
        // data: amount0In, amount1In, amount0Out, amount1Out
        if log.data.len() < 128 {
            return None;
        }
        let a0i = U256::from_be_slice(&log.data[0..32]);
        let a1i = U256::from_be_slice(&log.data[32..64]);
        let a0o = U256::from_be_slice(&log.data[64..96]);
        let a1o = U256::from_be_slice(&log.data[96..128]);
        if a0i.is_zero() && a1i.is_zero() && a0o.is_zero() && a1o.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V2, log.address);
        fact.amount_in = a0i.max(a1i);
        fact.amount_out = a0o.max(a1o);
        return Some((Amm::V2, fact));
    }

    if topic0 == V3_SWAP_TOPIC {
        // data: int256 amount0, int256 amount1, sqrtPriceX96, liquidity, tick
        if log.data.len() < 64 {
            return None;
        }
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V3, log.address);
        // amount > 0 = pool paid out (token out); amount < 0 = pool received (token in)
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
        }
        return Some((Amm::V3, fact));
    }

    if topic0 == *V4_SWAP_TOPIC {
        // topics: [sig, poolId]; data: int128 amount0, int128 amount1, ...
        if log.topics.len() < 2 || log.data.len() < 64 {
            return None;
        }
        let pool = Address::from_slice(&log.topics[1].as_slice()[12..]);
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V4, pool);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
        }
        return Some((Amm::V4, fact));
    }

    if topic0 == CURVE_TOKEN_EXCHANGE_TOPIC || topic0 == CURVE_V2_TOKEN_EXCHANGE_TOPIC {
        // data: int128 sold_id/coin_sold, uint256 amount_sold, int128 bought_id, uint256 amount_bought
        if log.data.len() < 128 {
            return None;
        }
        let mut fact = base(Amm::Curve, log.address);
        fact.amount_in = U256::from_be_slice(&log.data[32..64]);
        fact.amount_out = U256::from_be_slice(&log.data[96..128]);
        return Some((Amm::Curve, fact));
    }

    if topic0 == BALANCER_SWAP_TOPIC {
        // topics: [sig, poolId, tokenIn, tokenOut]; data: amountIn, amountOut
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        let mut fact = base(Amm::Balancer, log.address);
        fact.token_in = Address::from_slice(&log.topics[2].as_slice()[12..]);
        fact.token_out = Address::from_slice(&log.topics[3].as_slice()[12..]);
        fact.amount_in = U256::from_be_slice(&log.data[0..32]);
        fact.amount_out = U256::from_be_slice(&log.data[32..64]);
        return Some((Amm::Balancer, fact));
    }

    if topic0 == LB_SWAP_TOPIC {
        // topics: [sig, sender, tokenIn, tokenOut]; data: amountIn, amountOut
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        let mut fact = base(Amm::Lb, log.address);
        fact.token_in = Address::from_slice(&log.topics[2].as_slice()[12..]);
        fact.token_out = Address::from_slice(&log.topics[3].as_slice()[12..]);
        fact.amount_in = U256::from_be_slice(&log.data[0..32]);
        fact.amount_out = U256::from_be_slice(&log.data[32..64]);
        return Some((Amm::Lb, fact));
    }

    if topic0 == PENDLE_SWAP_TOPIC {
        // topics: [sig, caller, receiver]; data: int256 netPt, int256 netSy, ...
        if log.topics.len() < 3 || log.data.len() < 64 {
            return None;
        }
        let pt = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let sy = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if pt.is_zero() && sy.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Pendle, log.address);
        fact.amount_in = pt.wrapping_abs().into_raw();
        fact.amount_out = sy.wrapping_abs().into_raw();
        return Some((Amm::Pendle, fact));
    }

    if topic0 == *SOLIDLY_SWAP_TOPIC {
        // Solidly/Velodrome pool Swap(uint256,uint256,address,address):
        // data carries (amount0In, amount1In, amount0Out, amount1Out) packed
        // as two uint256 pairs in stable/volatile variants; treat like V2
        // magnitudes via transfer pairing for direction.
        if log.data.len() < 64 {
            return None;
        }
        let a0 = U256::from_be_slice(&log.data[0..32]);
        let a1 = U256::from_be_slice(&log.data[32..64]);
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Solidly, log.address);
        fact.amount_in = a0;
        fact.amount_out = a1;
        return Some((Amm::Solidly, fact));
    }

    None
}

/// Sentinel meaning "token0 of the pool" / "token1 of the pool" — resolved to
/// real addresses by transfer pairing (or pool registry when available).
pub const TOKEN0_SENTINEL: Address = Address::new([0xEEu8; 20]);
pub const TOKEN1_SENTINEL: Address = Address::new([0xE1u8; 20]);

/// Decode a liquidation fact via the per-protocol event registry.
/// Aave-style `LiquidationCall` and Compound V3 `Absorb` are covered; the
/// registry is intentionally not one hardcoded topic (plan §8.4).
pub fn decode_liquidation(log: &LogData) -> Option<LiquidationFact> {
    let topic0 = *log.topics.first()?;
    if topic0 == *AAVE_V3_LIQUIDATION_CALL_TOPIC {
        // topics: [sig, collateralAsset, debtAsset, user]
        // data: debtToCover, liquidatedCollateralAmount, receiveAToken
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            user: Address::from_slice(&log.topics[3][12..]),
            // `LiquidationCall` does not carry the liquidator (msg.sender);
            // attribution falls back to the tx sender at classify time.
            liquidator: Address::ZERO,
            collateral_asset: Address::from_slice(&log.topics[1][12..]),
            debt_asset: Address::from_slice(&log.topics[2][12..]),
            collateral_amount: U256::from_be_slice(&log.data[32..64]),
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
        });
    }
    if topic0 == *COMPOUND_V3_ABSORB_TOPIC {
        // topics: [sig, absorber]; data: (borrower[], basePaid[], basePaidTotal)
        if log.topics.len() < 2 || log.data.len() < 84 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v3",
            user: Address::from_slice(&log.data[12..32]),
            liquidator: Address::from_slice(&log.topics[1][12..]),
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_amount: U256::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[52..84]),
        });
    }
    None
}

/// Decode a V3 Mint/Burn fact (JIT candidate).
pub fn decode_v3_mint_burn(log: &LogData) -> Option<JitFact> {
    let topic0 = *log.topics.first()?;
    let is_mint = topic0 == V3_MINT_TOPIC;
    let is_burn = topic0 == V3_BURN_TOPIC;
    if !is_mint && !is_burn {
        return None;
    }
    // topics: [sig, sender, owner]; data: tickLower, tickUpper, amount, amount0, amount1
    if log.topics.len() < 3 || log.data.len() < 160 {
        return None;
    }
    let tick_lower = i32::from_be_bytes([
        log.data[28], log.data[29], log.data[30], log.data[31],
    ]);
    let tick_upper = i32::from_be_bytes([
        log.data[60], log.data[61], log.data[62], log.data[63],
    ]);
    let liquidity = crate::utils::u128_from_be_bytes(&log.data[64 + 16..96]);
    Some(JitFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        owner: Address::from_slice(&log.topics[2][12..]),
        tick_lower,
        tick_upper,
        is_mint,
        liquidity,
        amount0: U256::from_be_slice(&log.data[96..128]),
        amount1: U256::from_be_slice(&log.data[128..160]),
    })
}

/// Resolve swap token directions from the tx's transfer stream.
///
/// For each swap at log position `s.log_index` on pool `P`:
/// - token_in  = token of the nearest Transfer (lower log index) with `to == P`
/// - token_out = token of the nearest Transfer (higher log index) with `from == P`
///
/// This works across V2/V3/Curve/Solidly without pool metadata and resolves
/// the V3 sentinel direction when the paired transfer is unambiguous.
pub fn attach_swap_tokens(swaps: &mut [SwapFact], transfers: &[TransferFact]) {
    for s in swaps.iter_mut() {
        // Balancer/LB already carry explicit tokens from topics.
        if s.amm == Amm::Balancer || s.amm == Amm::Lb {
            continue;
        }
        let mut token_in = Address::ZERO;
        let mut token_out = Address::ZERO;
        let mut best_in: Option<i64> = None;
        let mut best_out: Option<i64> = None;
        for t in transfers {
            let dist = t.log_index as i64 - s.log_index as i64;
            if dist < 0 && t.to == s.pool {
                // nearest before
                match best_in {
                    Some(d) if d >= dist.abs() => {}
                    _ => {
                        best_in = Some(dist.abs());
                        token_in = t.token;
                    }
                }
            } else if dist > 0 && t.from == s.pool {
                match best_out {
                    Some(d) if d <= dist.abs() => {}
                    _ => {
                        best_out = Some(dist.abs());
                        token_out = t.token;
                    }
                }
            }
        }
        if !token_in.is_zero() {
            s.token_in = token_in;
        } else if s.token_in == TOKEN0_SENTINEL || s.token_in == TOKEN1_SENTINEL {
            // keep sentinel; classifier treats sentinel as unresolved
        }
        if !token_out.is_zero() {
            s.token_out = token_out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};

    fn log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> LogData {
        LogData {
            address,
            topics,
            data: alloy::primitives::Bytes::from(data),
        }
    }

    #[test]
    fn transfer_decodes() {
        let l = log(
            address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            vec![
                TRANSFER_TOPIC,
                b256!("0000000000000000000000000000000000000000000000000000000000000001"),
                b256!("0000000000000000000000000000000000000000000000000000000000000002"),
            ],
            {
                let mut b = vec![0u8; 32];
                b[31] = 42;
                b
            },
        );
        let t = decode_transfer(&l).unwrap();
        assert_eq!(t.token, address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"));
        assert_eq!(t.amount, U256::from(42));
    }

    #[test]
    fn v3_swap_decodes_direction() {
        let pool = address!("1111111111111111111111111111111111111111");
        // amount0 = -100 (pool received), amount1 = +90 (pool paid out)
        let mut data = vec![0u8; 160];
        for b in data[0..24].iter_mut() {
            *b = 0xff;
        }
        data[24..32].copy_from_slice(&(-100i64).to_be_bytes());
        data[56..64].copy_from_slice(&90u64.to_be_bytes());
        let l = log(pool, vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO], data);
        let (amm, s) = decode_swap(&l).unwrap();
        assert_eq!(amm, Amm::V3);
        assert_eq!(s.pool, pool);
        assert_eq!(s.amount_in, U256::from(100));
        assert_eq!(s.amount_out, U256::from(90));
        assert_eq!(s.token_in, TOKEN0_SENTINEL);
    }

    #[test]
    fn liquidation_aave_v3_decodes() {
        let collateral = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let debt = b256!("000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");
        let user = b256!("000000000000000000000000dddddddddddddddddddddddddddddddddddddddd");
        let mut data = vec![0u8; 64];
        data[24..32].copy_from_slice(&99u64.to_be_bytes()); // debtToCover
        data[56..64].copy_from_slice(&500u64.to_be_bytes()); // liquidatedCollateralAmount
        let l = log(
            address!("794a61358d6845594f94dc1db02a252b5b4814ad"),
            vec![*AAVE_V3_LIQUIDATION_CALL_TOPIC, collateral, debt, user],
            data,
        );
        let liq = decode_liquidation(&l).unwrap();
        assert_eq!(liq.protocol, "aave_v3");
        assert_eq!(liq.user, address!("dddddddddddddddddddddddddddddddddddddddd"));
        // liquidator is not in the event — resolved to tx.from at classify time
        assert_eq!(
            liq.liquidator,
            address!("0000000000000000000000000000000000000000")
        );
        assert_eq!(liq.debt_to_cover, U256::from(99));
        assert_eq!(liq.collateral_amount, U256::from(500));
    }

    #[test]
    fn attach_tokens_pairs_transfers() {
        let pool = address!("1000000000000000000000000000000000000000");
        let tin = address!("2000000000000000000000000000000000000000");
        let tout = address!("3000000000000000000000000000000000000000");
        let mk = |li: u64, token: Address, from: Address, to: Address| TransferFact {
            tx_index: 0,
            log_index: li,
            token,
            from,
            to,
            amount: U256::from(1),
        };
        let transfers = vec![
            mk(0, tin, address!("4000000000000000000000000000000000000000"), pool),
            mk(2, tout, pool, address!("4000000000000000000000000000000000000000")),
        ];
        let (_, mut s) = decode_swap(&log(
            pool,
            vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            {
                let mut d = vec![0u8; 128];
                d[31] = 5; // amount0In
                d[64 + 31] = 4; // amount0Out
                d
            },
        ))
        .unwrap();
        s.log_index = 1;
        attach_swap_tokens(std::slice::from_mut(&mut s), &transfers);
        assert_eq!(s.token_in, tin);
        assert_eq!(s.token_out, tout);
    }
}
