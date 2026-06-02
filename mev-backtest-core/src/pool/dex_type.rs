use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DexType {
    #[default]
    #[serde(rename = "uniswap_v2")]
    UniswapV2,
    #[serde(rename = "uniswap_v3")]
    UniswapV3,
    #[serde(rename = "curve")]
    Curve,
    #[serde(rename = "balancer")]
    Balancer,
}

impl DexType {
    pub fn is_concentrated_liquidity(self) -> bool {
        matches!(self, DexType::UniswapV3)
    }

    pub fn label(self) -> &'static str {
        match self {
            DexType::UniswapV2 => "UniswapV2",
            DexType::UniswapV3 => "UniswapV3",
            DexType::Curve => "Curve",
            DexType::Balancer => "Balancer",
        }
    }
}
