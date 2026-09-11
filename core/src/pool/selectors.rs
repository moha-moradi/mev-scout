//! Canonical 4-byte function selectors, one definition per ABI signature.
//!
//! Every selector is computed from its ABI signature at first use, so the
//! bytes are always consistent with the documented signature — this module
//! exists precisely because the codebase previously hard-coded byte arrays
//! that had drifted from (or never matched) their claimed signatures.
//!
//! Rule: a selector must be defined here exactly once, with its signature in
//! the name suffix where ambiguity exists (e.g. Curve pools expose both
//! `balances(int128)` on classic Vyper deployments and `balances(uint256)` on
//! NG deployments — call sites try both, see `fetch_curve_state`).

use std::sync::LazyLock;

use alloy::primitives::{keccak256, Bytes};

/// Compute the 4-byte selector of an ABI signature.
fn selector(sig: &str) -> Bytes {
    let hash = keccak256(sig.as_bytes());
    Bytes::copy_from_slice(&hash[..4])
}

macro_rules! selector {
    ($name:ident, $sig:literal) => {
        pub static $name: LazyLock<Bytes> = LazyLock::new(|| selector($sig));
    };
}

// ── Uniswap V2 / forks ────────────────────────────────────────────────────

selector!(GET_RESERVES, "getReserves()"); // 0x0902f1ac

// ── Uniswap V3 / V4 / Pancake Infinity ───────────────────────────────────

selector!(V3_SLOT0, "slot0()"); // 0x3850c7bd
selector!(V3_LIQUIDITY, "liquidity()"); // 0x1a686502
selector!(V3_TICK_BITMAP, "tickBitmap(int16)"); // 0x5339c296
selector!(V3_TICKS, "ticks(int24)"); // 0xf30dba93
selector!(INF_CL_SLOT0, "getSlot0(bytes32)"); // 0xc815641c — Pancake Infinity CL manager
selector!(INF_CL_LIQUIDITY, "getLiquidity(bytes32)"); // 0xfa6793d5 — Pancake Infinity CL manager

// ── Balancer V2 vault ────────────────────────────────────────────────────

selector!(BALANCER_GET_POOL, "getPool(address)"); // 0xbbe4f6db
selector!(GET_POOL_TOKENS, "getPoolTokens(bytes32)"); // 0xf94d4668
selector!(GET_NORMALIZED_WEIGHTS, "getNormalizedWeights()"); // 0xf89f27ed
selector!(GET_SWAP_FEE_PERCENTAGE, "getSwapFeePercentage()"); // 0x55c67628
selector!(GET_AMPLIFICATION_PARAMETER, "getAmplificationParameter()"); // 0x6daccffa
selector!(GET_SCALING_FACTORS, "getScalingFactors()"); // 0x1dd746ea
selector!(GET_RATE_PROVIDER, "getRateProvider(uint256)"); // 0x48ade7b0

// ── Curve ────────────────────────────────────────────────────────────────

selector!(CURVE_COINS_I128, "coins(int128)"); // 0x23746eb8 — classic Vyper pools
selector!(CURVE_COINS_U256, "coins(uint256)"); // 0xc6610657 — NG / some forks
selector!(CURVE_BALANCES_I128, "balances(int128)"); // 0x065a80d8 — classic Vyper pools
selector!(CURVE_BALANCES_U256, "balances(uint256)"); // 0x4903b0d1 — NG / some forks
selector!(CURVE_A, "A()"); // 0xf446c1d0 — StableSwap V1 amplification
selector!(CURVE_GET_A, "get_A()"); // 0xe91df8b3 — CryptoSwap V2 amplification
selector!(CURVE_GAMMA, "gamma()"); // 0xb1373929 — CryptoSwap V2
selector!(CURVE_PRICE_SCALE, "price_scale()"); // 0xb9e8c9fd — CryptoSwap V2
selector!(CURVE_BASE_POOL, "base_pool()"); // 0x5d6362bb — Metapools
selector!(CURVE_FEE, "fee()"); // 0xddca3f43 — shared with V3 fee()

// ── Trader Joe LB ────────────────────────────────────────────────────────

selector!(LB_GET_BIN, "getBin(uint256)"); // 0xfc7c59eb
selector!(LB_GET_BIN_STEP, "getBinStep()"); // 0x17f11ecc
selector!(LB_GET_ACTIVE_ID, "getActiveId()"); // 0xdbe65edc
selector!(TRADER_JOE_TOKEN_X, "tokenX()"); // 0x16dc165b — token0()/token1() revert on LBPair
selector!(TRADER_JOE_TOKEN_Y, "tokenY()"); // 0xb7d19fc4 — token0()/token1() revert on LBPair

// ── Pendle ───────────────────────────────────────────────────────────────

selector!(PENDLE_READ_STATE, "readState(address)"); // 0x794052f3
selector!(PENDLE_SY, "SY()"); // 0xafd27bf5 — on the PT token
selector!(PENDLE_READ_TOKENS, "readTokens()"); // returns (SY, PT, YT)

// ── ERC-20 / pool metadata ───────────────────────────────────────────────

selector!(TOKEN0, "token0()"); // 0x0dfe1681
selector!(TOKEN1, "token1()"); // 0xd21220a7
selector!(FEE, "fee()"); // 0xddca3f43 — V3/V4 fee
selector!(TICK_SPACING, "tickSpacing()"); // 0xd0c93a7c
selector!(SYMBOL, "symbol()"); // 0x95d89b41 — ERC-20

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin every selector to its independently computed keccak value so a
    /// signature edit cannot silently change the wire bytes.
    #[test]
    fn selectors_match_their_signatures() {
        let cases: &[(&str, &LazyLock<Bytes>, &str)] = &[
            ("getReserves()", &GET_RESERVES, "0902f1ac"),
            ("slot0()", &V3_SLOT0, "3850c7bd"),
            ("liquidity()", &V3_LIQUIDITY, "1a686502"),
            ("tickBitmap(int16)", &V3_TICK_BITMAP, "5339c296"),
            ("ticks(int24)", &V3_TICKS, "f30dba93"),
            ("getSlot0(bytes32)", &INF_CL_SLOT0, "c815641c"),
            ("getLiquidity(bytes32)", &INF_CL_LIQUIDITY, "fa6793d5"),
            ("getPool(address)", &BALANCER_GET_POOL, "bbe4f6db"),
            ("getPoolTokens(bytes32)", &GET_POOL_TOKENS, "f94d4668"),
            ("getNormalizedWeights()", &GET_NORMALIZED_WEIGHTS, "f89f27ed"),
            ("getSwapFeePercentage()", &GET_SWAP_FEE_PERCENTAGE, "55c67628"),
            (
                "getAmplificationParameter()",
                &GET_AMPLIFICATION_PARAMETER,
                "6daccffa",
            ),
            ("getScalingFactors()", &GET_SCALING_FACTORS, "1dd746ea"),
            ("getRateProvider(uint256)", &GET_RATE_PROVIDER, "48ade7b0"),
            ("coins(int128)", &CURVE_COINS_I128, "23746eb8"),
            ("coins(uint256)", &CURVE_COINS_U256, "c6610657"),
            ("balances(int128)", &CURVE_BALANCES_I128, "065a80d8"),
            ("balances(uint256)", &CURVE_BALANCES_U256, "4903b0d1"),
            ("A()", &CURVE_A, "f446c1d0"),
            ("get_A()", &CURVE_GET_A, "e91df8b3"),
            ("gamma()", &CURVE_GAMMA, "b1373929"),
            ("price_scale()", &CURVE_PRICE_SCALE, "b9e8c9fd"),
            ("base_pool()", &CURVE_BASE_POOL, "5d6362bb"),
            ("fee()", &CURVE_FEE, "ddca3f43"),
            ("getBin(uint256)", &LB_GET_BIN, "fc7c59eb"),
            ("getBinStep()", &LB_GET_BIN_STEP, "17f11ecc"),
            ("getActiveId()", &LB_GET_ACTIVE_ID, "dbe65edc"),
            ("tokenX()", &TRADER_JOE_TOKEN_X, "16dc165b"),
            ("tokenY()", &TRADER_JOE_TOKEN_Y, "b7d19fc4"),
            ("readState(address)", &PENDLE_READ_STATE, "794052f3"),
            ("SY()", &PENDLE_SY, "afd27bf5"),
            ("readTokens()", &PENDLE_READ_TOKENS, "2c8ce6bc"),
            ("token0()", &TOKEN0, "0dfe1681"),
            ("token1()", &TOKEN1, "d21220a7"),
            ("fee()", &FEE, "ddca3f43"),
            ("tickSpacing()", &TICK_SPACING, "d0c93a7c"),
            ("symbol()", &SYMBOL, "95d89b41"),
        ];
        for (sig, lock, expected_hex) in cases {
            let actual = hex::encode(&**lock);
            assert_eq!(
                &actual, expected_hex,
                "selector for {sig} drifted: expected {expected_hex}, got {actual}"
            );
        }
    }
}
