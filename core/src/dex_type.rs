//! DEX type enum (UniswapV2, UniswapV3, UniswapV4, Solidly, Camelot, Curve, Balancer,
//! TraderJoeLB, Pendle, PancakeInfinity, Metric, Fluid) and associated metadata.

use serde::{Deserialize, Serialize};

#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, strum::Display, strum::EnumString)]
#[strum(ascii_case_insensitive)]
pub enum DexType {
    #[default]
    #[serde(rename = "uniswap_v2")]
    #[strum(serialize = "UniswapV2")]
    UniswapV2 = 0,
    #[serde(rename = "uniswap_v3")]
    #[strum(serialize = "UniswapV3")]
    UniswapV3 = 1,
    #[serde(rename = "curve")]
    #[strum(serialize = "Curve")]
    Curve = 2,
    #[serde(rename = "balancer")]
    #[strum(serialize = "Balancer")]
    Balancer = 3,
    #[serde(rename = "solidly")]
    #[strum(serialize = "Solidly")]
    Solidly = 5,
    #[serde(rename = "camelot")]
    #[strum(serialize = "Camelot")]
    Camelot = 6,
    #[serde(rename = "uniswap_v4")]
    #[strum(serialize = "UniswapV4")]
    UniswapV4 = 7,
    #[serde(rename = "trader_joe_lb")]
    #[strum(serialize = "TraderJoeLB")]
    TraderJoeLB = 8,
    #[serde(rename = "pendle")]
    #[strum(serialize = "Pendle")]
    Pendle = 9,
    #[serde(rename = "pancake_infinity")]
    #[strum(serialize = "PancakeInfinity")]
    PancakeInfinity = 10,
    /// Metric V2 — oracle-anchored tick/bin AMM (single factory on 10 chains).
    #[serde(rename = "metric")]
    #[strum(serialize = "Metric")]
    Metric = 11,
    /// Fluid DEX (Instadapp) — unified-liquidity pools; price via eth_call centerPrice.
    #[serde(rename = "fluid")]
    #[strum(serialize = "Fluid")]
    Fluid = 12,
}
