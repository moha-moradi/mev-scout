//! Resolve DEX factory / vault addresses from [`ChainConfig`] overlays and
//! per-chain defaults into a single owned struct that can build a
//! [`DiscoveryConfig`](super::DiscoveryConfig).

use alloy::primitives::Address;

use super::{pick_factories, DiscoveryConfig};
use crate::cache::{SqliteStore, TokenCache};
use crate::config::ChainConfig;
use crate::types::ChainName;

/// Factory / vault addresses resolved from chain config overlaid with the
/// bundled per-chain defaults. Owns the vectors so [`Self::discovery_config`]
/// can hand out borrowed slices without re-parsing hex strings at each call site.
#[derive(Debug, Clone)]
pub struct ResolvedFactories {
    pub vault: Option<Address>,
    pub registry: Option<Address>,
    pub curve_factories: Vec<Address>,
    pub v2_factories: Vec<Address>,
    pub v3_factories: Vec<Address>,
    pub solidly_factories: Vec<Address>,
    pub camelot_factories: Vec<Address>,
    pub v4_pool_manager: Option<Address>,
    pub infinity_cl_pool_manager: Option<Address>,
    pub trader_joe_factories: Vec<Address>,
    pub pendle_factory: Option<Address>,
    pub metric_factory: Option<Address>,
    pub fluid_factory: Option<Address>,
    pub v2_fee_override: Option<u32>,
}

impl ResolvedFactories {
    /// Build from a typed [`ChainConfig`], falling back to chain defaults when
    /// a factory list is empty / unset.
    pub fn from_chain_config(chain_config: &ChainConfig, chain_name: ChainName) -> Self {
        let list = |configured: &Option<Vec<Address>>, defaults: &[&str]| {
            pick_factories(configured.clone().unwrap_or_default(), defaults)
        };
        Self {
            vault: chain_config.balancer_vault,
            registry: chain_config.curve_registry,
            curve_factories: list(
                &chain_config.curve_factories,
                chain_name.default_curve_factories(),
            ),
            v2_factories: list(
                &chain_config.uniswap_v2_factories,
                chain_name.default_uniswap_v2_factories(),
            ),
            v3_factories: list(
                &chain_config.uniswap_v3_factories,
                chain_name.default_uniswap_v3_factories(),
            ),
            solidly_factories: list(
                &chain_config.solidly_factories,
                &chain_name.default_solidly_factories(),
            ),
            camelot_factories: list(
                &chain_config.camelot_factories,
                &chain_name.default_camelot_factories(),
            ),
            v4_pool_manager: chain_config.v4_pool_manager,
            infinity_cl_pool_manager: chain_config.infinity_cl_pool_manager,
            trader_joe_factories: list(
                &chain_config.trader_joe_factories,
                &chain_name.default_trader_joe_factories(),
            ),
            pendle_factory: chain_config.pendle_factory,
            metric_factory: chain_config.metric_factory,
            fluid_factory: chain_config.fluid_factory,
            v2_fee_override: chain_config.uniswap_v2_default_fee,
        }
    }

    /// Borrow slices into a [`DiscoveryConfig`] with the given runtime knobs.
    pub fn discovery_config<'a>(&'a self, opts: DiscoveryRuntimeOpts<'a>) -> DiscoveryConfig<'a> {
        let slices = |v: &'a [Address]| {
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        };
        DiscoveryConfig {
            batch_size: opts.batch_size,
            v2_fee_override: self.v2_fee_override,
            balancer_vault: self.vault,
            v2_factories: slices(&self.v2_factories),
            v3_factories: slices(&self.v3_factories),
            curve_registry: self.registry,
            curve_factories: slices(&self.curve_factories),
            solidly_factories: slices(&self.solidly_factories),
            camelot_factories: slices(&self.camelot_factories),
            solidly_fee_bps: opts.solidly_fee_bps,
            v4_pool_manager: self.v4_pool_manager,
            infinity_cl_pool_manager: self.infinity_cl_pool_manager,
            trader_joe_factories: slices(&self.trader_joe_factories),
            pendle_factory: self.pendle_factory,
            metric_factory: self.metric_factory,
            fluid_factory: self.fluid_factory,
            rpc_concurrency: opts.rpc_concurrency,
            token_cache: opts.token_cache,
            pool_cache: opts.pool_cache,
        }
    }

    /// Log a one-line factory summary when any venue is configured (human mode).
    pub fn log_summary(&self, quiet: bool) {
        if quiet
            || (self.v2_factories.is_empty()
                && self.v3_factories.is_empty()
                && self.vault.is_none()
                && self.registry.is_none()
                && self.solidly_factories.is_empty()
                && self.camelot_factories.is_empty())
        {
            return;
        }
        tracing::info!(
            "Factories: {} V2, {} V3, {} Solidly, {} Camelot, Balancer: {}, Curve: {}",
            self.v2_factories.len(),
            self.v3_factories.len(),
            self.solidly_factories.len(),
            self.camelot_factories.len(),
            self.vault.is_some(),
            self.registry.is_some(),
        );
    }
}

/// Runtime knobs that sit alongside the resolved factory addresses when
/// building a [`DiscoveryConfig`].
#[derive(Clone, Copy)]
pub struct DiscoveryRuntimeOpts<'a> {
    pub batch_size: u64,
    pub solidly_fee_bps: Option<u32>,
    pub rpc_concurrency: usize,
    pub token_cache: Option<&'a TokenCache>,
    pub pool_cache: Option<&'a SqliteStore>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn prefers_configured_factory_list() {
        let mut cfg = ChainConfig {
            chain_id: 137,
            ..Default::default()
        };
        let custom = address!("0x1111111111111111111111111111111111111111");
        cfg.uniswap_v2_factories = Some(vec![custom]);
        let resolved = ResolvedFactories::from_chain_config(&cfg, ChainName::Polygon);
        assert_eq!(resolved.v2_factories, vec![custom]);
    }
}
