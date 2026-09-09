//! Core type definitions: chain names, strategies, gas config, output formats, and flash loan providers.

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
    pub const fn new(
        url: &'static str,
        default_rps: f64,
        label: &'static str,
        archive: bool,
    ) -> Self {
        Self {
            url,
            default_rps,
            label,
            archive,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[strum(ascii_case_insensitive)]
pub enum ChainName {
    #[strum(serialize = "polygon")]
    Polygon,
    #[strum(serialize = "avalanche")]
    Avalanche,
    #[strum(serialize = "bsc")]
    Bsc,
    #[strum(serialize = "arbitrum")]
    Arbitrum,
    #[strum(serialize = "base")]
    Base,
    #[strum(serialize = "ethereum")]
    Ethereum,
    #[strum(serialize = "optimism")]
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
                ProviderEndpoint::new(
                    "https://polygon-bor-rpc.publicnode.com",
                    1.0,
                    "publicnode",
                    true,
                ),
                ProviderEndpoint::new(
                    "https://polygon.api.onfinality.io/public",
                    0.5,
                    "onfinality",
                    true,
                ),
                ProviderEndpoint::new(
                    "https://rpc.satelink.network/rpc/polygon",
                    0.9,
                    "satelink",
                    true,
                ),
                ProviderEndpoint::new("https://api.zan.top/polygon-mainnet", 0.5, "zan", false),
                ProviderEndpoint::new("https://poly.api.pocket.network", 0.5, "pocket", false),
            ],
            ChainName::Avalanche => vec![ProviderEndpoint::new(
                "https://avalanche-c-chain.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
            ChainName::Bsc => vec![ProviderEndpoint::new(
                "https://bsc.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
            ChainName::Arbitrum => vec![ProviderEndpoint::new(
                "https://arbitrum-one.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
            ChainName::Base => vec![ProviderEndpoint::new(
                "https://base.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
            ChainName::Ethereum => vec![ProviderEndpoint::new(
                "https://ethereum-rpc.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
            ChainName::Optimism => vec![ProviderEndpoint::new(
                "https://optimism-rpc.publicnode.com",
                1.0,
                "publicnode",
                true,
            )],
        }
    }

    /// Primary public (free-tier) RPC endpoint — shortcut for `public_rpc_endpoints()[0].url`.
    pub fn public_rpc_url(&self) -> &'static str {
        self.public_rpc_endpoints()[0].url
    }

    /// Default Uniswap V2 factory addresses for this chain (built-in, no config file needed).
    pub fn default_uniswap_v2_factories(&self) -> &'static [&'static str] {
        match self {
            ChainName::Polygon => &[
                "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32", // QuickSwap
                "0xc35DADB65012eC5796536bD9864eD8773aBc74C4", // SushiSwap
                "0xCf083Be4164828f00cAE704EC15a36D711491284", // ApeSwap
                "0xE7Fb3e833eFE5F9c441105EB65Ef8b261266423B", // DFYN
                "0x9f3044f7f9fc8bc9ed615d54845b4577b833282d", // Meshswap
            ],
            ChainName::Avalanche => &[
                "0xc35DADB65012eC5796536bD9864eD8773aBc74C4", // SushiSwap
                "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10", // Trader Joe V1
            ],
            ChainName::Bsc => &[
                "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73", // PancakeSwap V2
                "0xc35DADB65012eC5796536bD9864eD8773aBc74C4", // SushiSwap
                "0x858E3312ed3A876947EA49d572A7C42DE08af7EE", // BiSwap
                "0x28F5E6C71C7541b1C6523351AE331CcAfC443626", // ListaV2 (UniV2 fork)
            ],
            ChainName::Arbitrum => &[], // Camelot handled via default_camelot_factories
            ChainName::Base => &[
                "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6", // Uniswap V2
            ], // Aerodrome handled via default_solidly_factories
            ChainName::Ethereum => &[
                "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f", // Uniswap V2
                "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac", // SushiSwap
                "0x115934131916C8b277DD010Ee02de363c09d037c", // ShibaSwap
            ],
            ChainName::Optimism => &[
                "0xFbc12984689e5f15626Bad03Ad60160Fe98B303C", // SushiSwap
            ],
        }
    }

    /// Default Uniswap V3 factory addresses for this chain.
    pub fn default_uniswap_v3_factories(&self) -> &'static [&'static str] {
        match self {
            ChainName::Polygon => &[
                "0x1F98431c8aD98523631AE4a59f267346ea31F984", // Uniswap V3
                "0x08958a3a1324f4870eb0028f1e93b2e3d8d78e09", // QuickSwap V3
                "0x2Bef16A0081565E72100D73CBe19B1Bd2d802380", // RamsesX (Ramses Exchange CL V2)
            ],
            ChainName::Avalanche => &[
                "0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD", // Uniswap V3
                "0xAE6E5c62328ade73ceefD42228528b70c8157D0d", // Pharaoh Exchange V3 (RamsesV3Factory)
                "0x1128F23D0bc0A8396E9FBC3c0c68f5EA228B8256", // Pangolin V3 (UniV3-fork, dynamic fee)
                "0x512eb749541B7cf294be882D636218c84a5e9E5F", // Blackhole CLMM (Algebra Integral)
            ],
            ChainName::Bsc => &[
                "0xdB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7", // PancakeSwap V3
                "0xcb010ed373523942706F730b89792aA1C1597b20", // ListaV3 (UniV3 fork)
            ],
            ChainName::Arbitrum => &[
                "0x1F98431c8aD98523631AE4a59f267346ea31F984", // Uniswap V3
                "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865", // PancakeSwap V3
                "0xd0019e86edB35E1fedaaB03aED5c3c60f115d28b", // Ramses V3 (CL)
            ],
            ChainName::Base => &[
                "0x33128a8fC17869897dcE68Ed026d694621f6FDfD", // Uniswap V3
                "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865", // PancakeSwap V3
                "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A", // Aerodrome Slipstream CL
                "0xaDe65c38CD4849aDBA595a4323a8C7DdfE89716a", // Aerodrome Slipstream CL (v2)
            ],
            ChainName::Ethereum => &[
                "0x1F98431c8aD98523631AE4a59f267346ea31F984", // Uniswap V3
                "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865", // PancakeSwap V3
            ],
            ChainName::Optimism => &[
                "0x1F98431c8aD98523631AE4a59f267346ea31F984", // Uniswap V3
                "0x548118C7E0B865C2CfA94D15EC86B666468ac758", // Velodrome V3 CL
                "0xCc0bDDB707055e04e497aB22a59c2aF4391cd12F", // Velodrome V3 CL (v2)
            ],
        }
    }

    /// Solidly-style factory addresses (PairCreated with bool stable).
    /// Velodrome (Optimism), Aerodrome (Base), Equalizer, Thena, etc.
    pub fn default_solidly_factories(&self) -> Vec<&'static str> {
        match self {
            ChainName::Base => vec![
                "0x8909Dc15e40173Ff4699343b6eB8132c0eE88a14", // Aerodrome
            ],
            ChainName::Optimism => vec![
                "0x420DD381b31aEf6683db6B902084cB0FFECe40Da", // Velodrome V2
            ],
            _ => vec![],
        }
    }

    /// Trader Joe / LFJ V2 LB factory addresses (raw + LB, V2.1 + V2.2 coexist).
    pub fn default_trader_joe_factories(&self) -> Vec<&'static str> {
        match self {
            ChainName::Avalanche => vec![
                "0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c", // LFJ V2.2
                "0x8e42f2F4101563bF679975178e880FD87d3eFd4e", // LFJ V2.1
                "0xEb480050b016f6c6d45203D2346B68bDDDa23D4D", // Pharaoh DLMM (LB v2.1-compatible)
            ],
            ChainName::Arbitrum => vec![
                "0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c", // LFJ V2.2
                "0x8e42f2F4101563bF679975178e880FD87d3eFd4e", // LFJ V2.1
            ],
            ChainName::Bsc => vec![
                "0x8e42f2F4101563bF679975178e880FD87d3eFd4e", // LFJ V2.1
            ],
            ChainName::Ethereum => vec![
                "0xDC8d77b69155c7E68A95a4fb0f06a71FF90B943a", // LFJ V2.1
            ],
            _ => vec![],
        }
    }

    /// Curve stableswap factory addresses (CurveStableswapFactoryNG deployments
    /// emitting `PoolDeployed(address)`; older factories emit `PoolAdded(...)`).
    pub fn default_curve_factories(&self) -> &'static [&'static str] {
        match self {
            ChainName::Polygon => &["0x1764ee18e8B3ccA4787249Ceb249356192594585"],
            ChainName::Bsc => &["0xd7E72f3615aa65b92A4DBdC211E296a35512988B"],
            ChainName::Arbitrum => &["0x9AF14D26075f142eb3F292D5065EB3faa646167b"],
            ChainName::Ethereum => &["0x6A8cbed756804B16E05E741eDaBd5cB544AE21bf"],
            ChainName::Optimism => &["0x5eeE3091f747E60a045a2E715a4c71e600e31F6E"],
            _ => &[],
        }
    }

    /// Camelot factory address (PairCreated with address,uint256,bool).
    pub fn default_camelot_factories(&self) -> Vec<&'static str> {
        match self {
            ChainName::Arbitrum => vec![
                "0x6EcCab422D763aC031210895C81787E87B43A652", // Camelot
            ],
            _ => vec![],
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in factory literal must parse as a valid 20-byte address —
    /// guards against checksum/typo regressions in the tables above.
    #[test]
    fn all_default_factories_parse_as_addresses() {
        for (chain, label) in [
            (ChainName::Polygon, "polygon"),
            (ChainName::Avalanche, "avalanche"),
            (ChainName::Bsc, "bsc"),
            (ChainName::Arbitrum, "arbitrum"),
            (ChainName::Base, "base"),
            (ChainName::Ethereum, "ethereum"),
            (ChainName::Optimism, "optimism"),
        ] {
            for f in chain.default_uniswap_v2_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} v2 factory {f}: {e}"));
            }
            for f in chain.default_uniswap_v3_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} v3 factory {f}: {e}"));
            }
            for f in chain.default_solidly_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} solidly factory {f}: {e}"));
            }
            for f in chain.default_camelot_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} camelot factory {f}: {e}"));
            }
            for f in chain.default_trader_joe_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} trader-joe factory {f}: {e}"));
            }
            for f in chain.default_curve_factories() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{label} curve factory {f}: {e}"));
            }
        }
    }

    /// Phase D universe-breadth factories (plan 2026-08-25), pinned so silent
    /// removals surface in CI.
    #[test]
    fn phase_d_factories_present() {
        // PancakeSwap V3 (Arbitrum) — deterministic cross-chain deployment.
        assert!(ChainName::Arbitrum
            .default_uniswap_v3_factories()
            .contains(&"0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"));
        // Ramses V3 CL factory (Arbitrum) — docs.ramses.exchange contract list.
        assert!(ChainName::Arbitrum
            .default_uniswap_v3_factories()
            .contains(&"0xd0019e86edB35E1fedaaB03aED5c3c60f115d28b"));
        // BiSwap (BSC) — live UniV2-style fork (allPairsLength verified on-chain).
        assert!(ChainName::Bsc
            .default_uniswap_v2_factories()
            .contains(&"0x858E3312ed3A876947EA49d572A7C42DE08af7EE"));
        // Uniswap V2 (Base) — official v2 deployments table.
        assert!(ChainName::Base
            .default_uniswap_v2_factories()
            .contains(&"0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"));
    }

    /// Phase 1.3 / 1.4 V3-family factories (DEX_COVERAGE_PLAN.md), pinned so a
    /// silent removal from the effective default list surfaces in CI. Both are
    /// verified RamsesV3Factory deployments (canonical UniV3 `PoolCreated`
    /// topic); RamsesX on Polygon, Pharaoh V3 on Avalanche.
    #[test]
    fn coverage_plan_v3_factories_present() {
        assert!(ChainName::Polygon
            .default_uniswap_v3_factories()
            .contains(&"0x2Bef16A0081565E72100D73CBe19B1Bd2d802380")); // RamsesX
        assert!(ChainName::Avalanche
            .default_uniswap_v3_factories()
            .contains(&"0xAE6E5c62328ade73ceefD42228528b70c8157D0d")); // Pharaoh V3
        assert!(ChainName::Avalanche
            .default_uniswap_v3_factories()
            .contains(&"0x1128F23D0bc0A8396E9FBC3c0c68f5EA228B8256")); // Pangolin V3
        assert!(ChainName::Avalanche
            .default_uniswap_v3_factories()
            .contains(&"0x512eb749541B7cf294be882D636218c84a5e9E5F")); // Blackhole CLMM (Algebra)
    }

    /// Coverage-plan LB and Curve factories (Phases 1.5/1.7/3.5), pinned so silent
    /// removals from the effective default lists surface in CI.
    #[test]
    fn coverage_plan_lb_and_curve_factories_present() {
        assert!(ChainName::Avalanche
            .default_trader_joe_factories()
            .contains(&"0xEb480050b016f6c6d45203D2346B68bDDDa23D4D")); // Pharaoh DLMM
        assert!(ChainName::Bsc
            .default_curve_factories()
            .contains(&"0xd7E72f3615aa65b92A4DBdC211E296a35512988B"));
        assert!(ChainName::Polygon
            .default_curve_factories()
            .contains(&"0x1764ee18e8B3ccA4787249Ceb249356192594585"));
    }
}
