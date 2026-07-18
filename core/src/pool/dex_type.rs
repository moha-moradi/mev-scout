//! DEX type enum (UniswapV2, UniswapV3, UniswapV4, Solidly, Camelot, Curve, Balancer,
//! TraderJoeLB, Pendle) and associated metadata.

use serde::{Deserialize, Serialize};

#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DexType {
    #[default]
    #[serde(rename = "uniswap_v2")]
    UniswapV2 = 0,
    #[serde(rename = "uniswap_v3")]
    UniswapV3 = 1,
    #[serde(rename = "curve")]
    Curve = 2,
    #[serde(rename = "balancer")]
    Balancer = 3,
    #[serde(rename = "dodo")]
    Dodo = 4,
    #[serde(rename = "solidly")]
    Solidly = 5,
    #[serde(rename = "camelot")]
    Camelot = 6,
    #[serde(rename = "uniswap_v4")]
    UniswapV4 = 7,
    #[serde(rename = "trader_joe_lb")]
    TraderJoeLB = 8,
    #[serde(rename = "pendle")]
    Pendle = 9,
}

impl DexType {
    pub fn is_concentrated_liquidity(self) -> bool {
        matches!(self, DexType::UniswapV3 | DexType::UniswapV4)
    }

    pub fn label(self) -> &'static str {
        match self {
            DexType::UniswapV2 => "UniswapV2",
            DexType::UniswapV3 => "UniswapV3",
            DexType::Solidly => "Solidly",
            DexType::Camelot => "Camelot",
            DexType::Curve => "Curve",
            DexType::Balancer => "Balancer",
            DexType::UniswapV4 => "UniswapV4",
            DexType::Dodo => "Dodo",
            DexType::TraderJoeLB => "TraderJoeLB",
            DexType::Pendle => "Pendle",
        }
    }
}
