use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;

use crate::cache::SqliteStore;
use crate::config::Config;
use crate::dex_type::DexType;
use crate::pool::discovery::is_epoch_venue_factory;
use crate::pool::PoolInfo;
use crate::types::ChainName;

/// Pool → (token0, token1) plus skim-eligible V2-like and epoch-venue sets.
#[derive(Debug, Clone, Default)]
pub struct PoolRegistry {
    pub tokens: HashMap<Address, (Address, Address)>,
    pub v2_like: HashSet<Address>,
    /// Pharaoh / Blackhole pools for plan P3.15 epoch tagging.
    pub epoch_venue: HashSet<Address>,
}

fn is_v2_like(dex: DexType) -> bool {
    matches!(
        dex,
        DexType::UniswapV2 | DexType::Solidly | DexType::Camelot
    )
}

/// Skim needs a V2-like type and a verified creator. Remote pools whose
/// metadata could not be resolved historically defaulted to `UniswapV2` with
/// no factory; treating those as skim-eligible converted CL/LB swap activity
/// into large false-positive skims.
fn is_skim_eligible(pool: &PoolInfo) -> bool {
    is_v2_like(pool.dex_type) && pool.factory.is_some()
}

/// Load the pool → (token0, token1) registry and V2-like skim set from the
/// scanner cache. Best-effort: an absent/locked cache yields empty
/// maps and ingest degrades to transfer-pairing resolution (no skim).
pub fn load_pool_registry(config: &Config, chain: &ChainName) -> PoolRegistry {
    let mut tokens = HashMap::new();
    let mut v2_like = HashSet::new();
    let mut epoch_venue = HashSet::new();
    match SqliteStore::open(config.effective_db_path(chain)) {
        Ok(cache) => match cache.list_discovered_pools() {
            Ok(pools) => {
                for p in pools {
                    if !p.token0.is_zero() && !p.token1.is_zero() {
                        tokens.insert(p.address, (p.token0, p.token1));
                        if is_skim_eligible(&p) {
                            v2_like.insert(p.address);
                        }
                        if p.factory.is_some_and(is_epoch_venue_factory) {
                            epoch_venue.insert(p.address);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("pool registry load failed: {e}"),
        },
        Err(e) => tracing::warn!("pool registry open failed: {e}"),
    }
    PoolRegistry {
        tokens,
        v2_like,
        epoch_venue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skim_eligibility_requires_v2_type_and_verified_factory() {
        let mut pool = PoolInfo {
            address: Address::from([1u8; 20]),
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            dex_type: DexType::UniswapV2,
            ..Default::default()
        };
        assert!(
            !is_skim_eligible(&pool),
            "unresolved remote V2 metadata must not enable skim"
        );
        pool.factory = Some(Address::from([4u8; 20]));
        assert!(is_skim_eligible(&pool));
        pool.dex_type = DexType::TraderJoeLB;
        assert!(!is_skim_eligible(&pool), "LB pools are not V2 skim venues");
    }
}
