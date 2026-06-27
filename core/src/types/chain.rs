//! Core type definitions: chain names, strategies, gas config, output formats, and flash loan providers.

use std::fmt;
use std::str::FromStr;
use alloy::primitives::Address;

/// A known public RPC endpoint with metadata for rate-limit-aware load distribution.
#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    pub url: &'static str,
    /// Recommended safe requests-per-second. Derived from observed public-tier limits.
    pub default_rps: f64,
    /// Human-readable label (e.g. "publicnode", "sentio").
    pub label: &'static str,
    /// Whether this endpoint has been verified to support `eth_getProof` (archive).
    pub archive: bool,
}

impl ProviderEndpoint {
    pub const fn new(url: &'static str, default_rps: f64, label: &'static str, archive: bool) -> Self {
        Self { url, default_rps, label, archive }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChainName {
    Polygon,
    Avalanche,
    Bsc,
    Arbitrum,
    Base,
    Ethereum,
    Optimism,
}

impl ChainName {
    pub fn chain_id(self) -> u64 {
        match self {
            ChainName::Polygon => 137,
            ChainName::Avalanche => 43114,
            ChainName::Bsc => 56,
            ChainName::Arbitrum => 42161,
            ChainName::Base => 8453,
            ChainName::Ethereum => 1,
            ChainName::Optimism => 10,
        }
    }

    /// Public (free-tier) RPC endpoints with metadata (URL, RPS, archive support).
    ///
    /// Each entry includes an observed safe RPS for public-tier usage.
    /// Endpoints are ordered by preference (fastest / most reliable first).
    pub fn public_rpc_endpoints(&self) -> Vec<ProviderEndpoint> {
        match self {
            ChainName::Polygon => vec![
                ProviderEndpoint::new("https://polygon.lava.build", 0.9, "lava", true),
                ProviderEndpoint::new("https://rpc.sentio.xyz/matic", 0.6, "sentio", true),
                ProviderEndpoint::new("https://matic.rpc.sentio.xyz", 0.8, "sentio-alt", true),
                ProviderEndpoint::new("https://polygon-bor-rpc.publicnode.com", 1.0, "publicnode", true),
                ProviderEndpoint::new("https://polygon.api.onfinality.io/public", 0.5, "onfinality", true),
                ProviderEndpoint::new("https://rpc.satelink.network/rpc/polygon", 0.9, "satelink", true),
                ProviderEndpoint::new("https://api.zan.top/polygon-mainnet", 0.5, "zan", false),
                ProviderEndpoint::new("https://poly.api.pocket.network", 0.5, "pocket", false),
            ],
            ChainName::Avalanche => vec![
                ProviderEndpoint::new("https://avalanche-c-chain.publicnode.com", 1.0, "publicnode", true),
            ],
            ChainName::Bsc => vec![
                ProviderEndpoint::new("https://bsc.publicnode.com", 1.0, "publicnode", true),
            ],
            ChainName::Arbitrum => vec![
                ProviderEndpoint::new("https://arbitrum-one.publicnode.com", 1.0, "publicnode", true),
            ],
            ChainName::Base => vec![
                ProviderEndpoint::new("https://base.publicnode.com", 1.0, "publicnode", true),
            ],
            ChainName::Ethereum => vec![
                ProviderEndpoint::new("https://ethereum-rpc.publicnode.com", 1.0, "publicnode", true),
            ],
            ChainName::Optimism => vec![
                ProviderEndpoint::new("https://optimism-rpc.publicnode.com", 1.0, "publicnode", true),
            ],
        }
    }

    /// Public (free-tier) RPC URLs — shortcut extracting URLs from `public_rpc_endpoints()`.
    pub fn public_rpc_urls(&self) -> Vec<&'static str> {
        self.public_rpc_endpoints().iter().map(|e| e.url).collect()
    }

    /// Primary public (free-tier) RPC endpoint — shortcut for `public_rpc_endpoints()[0].url`.
    pub fn public_rpc_url(&self) -> &'static str {
        self.public_rpc_endpoints()[0].url
    }

    /// Default Uniswap V2 factory addresses for this chain (built-in, no config file needed).
    pub fn default_uniswap_v2_factories(&self) -> Vec<&'static str> {
        match self {
            ChainName::Polygon => vec![
                "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32", // QuickSwap
                "0xc35DADB65012eC5796536bD9864eD8773aBc74C4", // SushiSwap
                "0xCf083Be4164828f00cAE704EC15a36D711491284", // ApeSwap
                "0xE7Fb3e833eFE5F9c441105EB65Ef8b261266423B", // DFYN
                "0x9f3044f7f9fc8bc9ed615d54845b4577b833282d", // Meshswap
            ],
            ChainName::Avalanche => vec![
                "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b", // SushiSwap
                "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10", // Trader Joe V1
            ],
            ChainName::Bsc => vec![
                "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73", // PancakeSwap V2
                "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b", // SushiSwap
            ],
            ChainName::Arbitrum => vec![
                "0x6EcCab422D763aC031210895C81787E87B43A652", // Camelot V2
            ],
            ChainName::Base => vec![
                "0x8909Dc15e40173Ff4699343b6eB8132c0eE88a14", // Aerodrome
            ],
            ChainName::Ethereum => vec![
                "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f", // Uniswap V2
                "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac", // SushiSwap
                "0xB3e281E8c6c888A5BcBf1108E4aC13dA3F5B1c9", // ShibaSwap
            ],
            ChainName::Optimism => vec![
                "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b", // SushiSwap
            ],
        }
    }

    /// Default Uniswap V3 factory addresses for this chain.
    pub fn default_uniswap_v3_factories(&self) -> &'static [&'static str] {
        match self {
            ChainName::Polygon => &[
                "0x1F98431c8aD98523631AE4a59f267346ea31F984", // Uniswap V3
                "0x08958a3a1324f4870eb0028f1e93b2e3d8d78e09", // QuickSwap V3
            ],
            ChainName::Avalanche => &["0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD"],
            ChainName::Bsc => &["0xdB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7"],
            ChainName::Arbitrum => &["0x1F98431c8aD98523631AE4a59f267346ea31F984"],
            ChainName::Base => &["0x33128a8fC17869897dcE68Ed026d694621f6FDfD"],
            ChainName::Ethereum => &["0x1F98431c8aD98523631AE4a59f267346ea31F984"],
            ChainName::Optimism => &["0x1F98431c8aD98523631AE4a59f267346ea31F984"],
        }
    }

    pub fn all() -> &'static [ChainName] {
        &[
            ChainName::Polygon,
            ChainName::Avalanche,
            ChainName::Bsc,
            ChainName::Arbitrum,
            ChainName::Base,
            ChainName::Ethereum,
            ChainName::Optimism,
        ]
    }
}

/// Return the storage slot(s) to try for a V2 pool created by the given factory.
/// Returns `&[6]` (standard Uniswap V2) for unknown or standard factories.
/// Known forks:
/// - Camelot → slot 8
/// - Aerodrome / Velodrome → slots [6, 12]
pub fn v2_storage_slots_for_factory(factory: Option<Address>) -> &'static [u64] {
    use alloy::primitives::address;
    match factory {
        Some(addr) if addr == address!("6EcCab422D763aC031210895C81787E87B43A652") => {
            &[8] // Camelot
        }
        Some(addr)
            if addr == address!("8909Dc15e40173Ff4699343b6eB8132c0eE88a14")
                || addr == address!("420DD381b31aEf6683db6B902084cB0FFECe40Da") =>
        {
            &[6, 12] // Aerodrome / Velodrome
        }
        _ => &[6], // Standard Uniswap V2, PancakeSwap, QuickSwap, SushiSwap, etc.
    }
}

/// Return the known V2 router address for a given factory, if available.
/// Used by M3 on-chain simulation to call `getAmountsOut` via eth_call.
/// Returns `None` for unknown factories — caller falls back to structural formula.
pub fn v2_router_for_factory(factory: Address) -> Option<Address> {
    use alloy::primitives::address;
    // Uniswap V2 (Ethereum)
    if factory == address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f") {
        return Some(address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D"));
    }
    // QuickSwap (Polygon)
    if factory == address!("5757371414417b8C6CAad45bAeF941aBc7d3Ab32") {
        return Some(address!("a5E0829CaCEd8fFDD4De3c43696c57F7D7A678ff"));
    }
    // PancakeSwap V2 (BSC)
    if factory == address!("cA143Ce32Fe78f1f7019d7d551a6402fC5350c73") {
        return Some(address!("10ED43C718714eb63d5aA57B78B54704E256024E"));
    }
    // SushiSwap (Ethereum, Polygon)
    if factory == address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
        || factory == address!("c35DADB65012eC5796536bD9864eD8773aBc74C4")
    {
        return Some(address!("1b02dA8Cb0d097eB8D57A175b88c7D8b47997506"));
    }
    // Camelot (Arbitrum)
    if factory == address!("6EcCab422D763aC031210895C81787E87B43A652") {
        return Some(address!("c873fEcbd354f5A56E00E710B90EF4201db2448d"));
    }
    // Trader Joe (Avalanche)
    if factory == address!("9Ad6C38BE94206cA50bb0d90783181662f0Cfa10") {
        return Some(address!("60aE616a2155Ee3d9A68541Ba4544862310933d4"));
    }
    None
}

impl fmt::Display for ChainName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainName::Polygon => write!(f, "polygon"),
            ChainName::Avalanche => write!(f, "avalanche"),
            ChainName::Bsc => write!(f, "bsc"),
            ChainName::Arbitrum => write!(f, "arbitrum"),
            ChainName::Base => write!(f, "base"),
            ChainName::Ethereum => write!(f, "ethereum"),
            ChainName::Optimism => write!(f, "optimism"),
        }
    }
}

impl FromStr for ChainName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "polygon" => Ok(ChainName::Polygon),
            "avalanche" => Ok(ChainName::Avalanche),
            "bsc" => Ok(ChainName::Bsc),
            "arbitrum" => Ok(ChainName::Arbitrum),
            "base" => Ok(ChainName::Base),
            "ethereum" => Ok(ChainName::Ethereum),
            "optimism" => Ok(ChainName::Optimism),
            _ => Err(format!(
                "unknown chain '{s}'. Supported: {}",
                ChainName::all()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_name_roundtrip() {
        for chain in ChainName::all() {
            let s = chain.to_string();
            let parsed: ChainName = s.parse().unwrap();
            assert_eq!(*chain, parsed);
        }
    }

    #[test]
    fn test_chain_name_unknown() {
        let err = "unknown".parse::<ChainName>().unwrap_err();
        assert!(err.contains("unknown chain"));
    }

    #[test]
    fn test_chain_name_chain_id() {
        assert_eq!(ChainName::Polygon.chain_id(), 137);
        assert_eq!(ChainName::Ethereum.chain_id(), 1);
    }
}
