//! DEX type enum (UniswapV2, UniswapV3, UniswapV4, Solidly, Camelot, Curve, Balancer,
//! TraderJoeLB, Pendle) and associated metadata.

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
}

impl DexType {
    pub fn is_concentrated_liquidity(self) -> bool {
        matches!(self, DexType::UniswapV3 | DexType::UniswapV4)
    }
}
