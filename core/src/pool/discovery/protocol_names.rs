//! Factory address → human protocol name.
//!
//! On-chain discovery classifies pools by AMM family (`UniswapV2`, `UniswapV3`,
//! `TraderJoeLB`, …). Forks share the same event ABI, so without a factory map
//! every Avalanche Joe / Pangolin / Pharaoh pool is labeled "UniswapV2/V3".

use alloy::primitives::{address, Address};

/// Resolve a known factory to its protocol brand name.
pub fn protocol_name_for_factory(factory: Address) -> Option<&'static str> {
    // Keep lowercase hex for stable matching; Address equality is case-insensitive.
    match factory {
        // ── Avalanche ────────────────────────────────────────────────────
        // LFJ (Trader Joe) V1 classic AMM
        a if a == address!("0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10") => Some("LFJ V1"),
        // SushiSwap V2 (Avalanche)
        a if a == address!("0xc35DADB65012eC5796536bD9864eD8773aBc74C4") => Some("SushiSwap"),
        // Pangolin V2
        a if a == address!("0xefa94DE7a4656D787667C749f7E1223D71E9FD88") => Some("Pangolin V2"),
        // Uniswap V3 (canonical Avalanche deployment)
        a if a == address!("0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD") => Some("Uniswap V3"),
        // Pharaoh V3 (RamsesV3Factory)
        a if a == address!("0xAE6E5c62328ade73ceefD42228528b70c8157D0d") => Some("Pharaoh V3"),
        // Pangolin V3
        a if a == address!("0x1128F23D0bc0A8396E9FBC3c0c68f5EA228B8256") => Some("Pangolin V3"),
        // Curve Stableswap Factory NG (Avalanche / Polygon shared address)
        a if a == address!("0x1764ee18e8B3ccA4787249Ceb249356192594585") => Some("Curve Stableswap NG"),
        // Blackhole CLMM (Algebra-family)
        a if a == address!("0x512eb749541B7cf294be882D636218c84a5e9E5F") => Some("Blackhole CLMM"),
        // LFJ Liquidity Book V2.2 / V2.1 + Pharaoh DLMM
        a if a == address!("0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c") => Some("LFJ V2.2"),
        a if a == address!("0x8e42f2F4101563bF679975178e880FD87d3eFd4e") => Some("LFJ V2.1"),
        a if a == address!("0xEb480050b016f6c6d45203D2346B68bDDDa23D4D") => Some("Pharaoh DLMM"),
        // Uniswap V4 pool manager
        a if a == address!("0x06380c0e0912312b5150364b9dc4542ba0dbbc85") => Some("Uniswap V4"),
        // Metric V2 (deterministic factory across chains)
        a if a == address!("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C") => Some("Metric V2"),
        // Balancer V2 vault
        a if a == address!("0xBA12222222228d8Ba445958a75a0704d566BF2C8") => Some("Balancer V2"),

        _ => None,
    }
}

/// Prefer a factory-derived brand name; fall back to the AMM-family label.
pub fn resolve_dex_name(factory: Option<Address>, dex_type_label: &str) -> String {
    factory
        .and_then(protocol_name_for_factory)
        .map(str::to_string)
        .unwrap_or_else(|| dex_type_label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avalanche_factories_resolve() {
        assert_eq!(
            protocol_name_for_factory(address!("0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10")),
            Some("LFJ V1")
        );
        assert_eq!(
            protocol_name_for_factory(address!("0xAE6E5c62328ade73ceefD42228528b70c8157D0d")),
            Some("Pharaoh V3")
        );
        assert_eq!(
            protocol_name_for_factory(address!("0x1128F23D0bc0A8396E9FBC3c0c68f5EA228B8256")),
            Some("Pangolin V3")
        );
        assert_eq!(
            protocol_name_for_factory(address!("0xefa94DE7a4656D787667C749f7E1223D71E9FD88")),
            Some("Pangolin V2")
        );
        assert_eq!(
            protocol_name_for_factory(address!("0x0000000000000000000000000000000000000001")),
            None
        );
    }

    #[test]
    fn resolve_falls_back_to_dex_type() {
        assert_eq!(resolve_dex_name(None, "UniswapV2"), "UniswapV2");
        assert_eq!(
            resolve_dex_name(
                Some(address!("0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10")),
                "UniswapV2"
            ),
            "LFJ V1"
        );
    }
}
