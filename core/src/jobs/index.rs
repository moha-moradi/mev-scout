use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;

use crate::cache::SqliteStore;
use crate::config::Config;
use crate::dex_type::DexType;
use crate::pool::discovery::is_epoch_venue_factory;
use crate::pool::discovery::remote::infer_dex_type_strict;
use crate::pool::PoolInfo;
use crate::types::ChainName;

/// Bumped whenever `infer_dex_type` semantics change in a way that would
/// re-derive stored pool metadata. Re-exported from `dex_type` so pool
/// construction and the registry loader share one version.
pub use crate::dex_type::POOL_DERIVATION_VERSION;

/// Health of the stored pool registry, surfaced by [`load_pool_registry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegistryHealth {
    /// Rows whose stored `dex_type` disagreed with strict inference from the
    /// row's own `dex_name`.
    pub stale_derivations: usize,
    /// How many of those disagreements were rewritten in place this load.
    pub repaired: usize,
    /// Rows where strict inference would turn a non-V2 family into a V2-like
    /// one. Refused: a generic `dex_name` must never make skim *more* likely.
    pub downgrades_refused: usize,
    /// Rows stored as V2-like with no verified factory. They are already
    /// skim-ineligible (see [`is_skim_eligible`]) but show metadata that never
    /// resolved.
    pub unverified_v2_like: usize,
}

/// Pool → (token0, token1) plus skim-eligible V2-like and epoch-venue sets.
#[derive(Debug, Clone, Default)]
pub struct PoolRegistry {
    pub tokens: HashMap<Address, (Address, Address)>,
    pub v2_like: HashSet<Address>,
    /// Pharaoh / Blackhole pools for plan P3.15 epoch tagging.
    pub epoch_venue: HashSet<Address>,
    /// Metadata consistency counters; never consulted by classification.
    pub health: RegistryHealth,
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

/// Stored row vs. current strict inference, using only the row's own
/// `dex_name`. `None` when no name was recorded or the name does not
/// positively identify a family (never fall back to `UniswapV2` here — that is
/// how a `"Trader Joe LB"` row would be downgraded to a V2 skim venue).
fn rederived_dex_type(pool: &PoolInfo) -> Option<DexType> {
    let name = pool.dex_name.as_deref()?;
    infer_dex_type_strict(Some(name), &[])
}

/// Load the pool → (token0, token1) registry and V2-like skim set from the
/// scanner cache. Best-effort: an absent/locked cache yields empty
/// maps and ingest degrades to transfer-pairing resolution (no skim).
///
/// Rows inferred under an older rule set (`derivation_version` missing or
/// stale) are re-derived from their own `dex_name`. A row that positively
/// re-derives to a different family is repaired in place, so the stale
/// metadata cannot poison skim eligibility again on the next load.
pub fn load_pool_registry(config: &Config, chain: &ChainName) -> PoolRegistry {
    let mut tokens = HashMap::new();
    let mut v2_like = HashSet::new();
    let mut epoch_venue = HashSet::new();
    let mut health = RegistryHealth::default();
    // Bounded sample of rows whose skim-eligibility class would change.
    let mut stale: Vec<(Address, DexType, DexType)> = Vec::new();
    match SqliteStore::open(config.effective_db_path(chain)) {
        Ok(cache) => match cache.list_discovered_pools() {
            Ok(pools) => {
                for mut p in pools {
                    if p.token0.is_zero() || p.token1.is_zero() {
                        continue;
                    }
                    // Rows derived under current rules were already checked.
                    if p.derivation_version != Some(POOL_DERIVATION_VERSION) {
                        if let Some(derived) = rederived_dex_type(&p) {
                            if derived != p.dex_type {
                                health.stale_derivations += 1;
                                // Only act when the change removes skim
                                // exposure. Family-only drift is noise, and
                                // inferring V2 for a CL/LB row (e.g. a bare
                                // generic "pharaoh" name on a V3 pool) must
                                // never make skim more likely.
                                match (is_v2_like(p.dex_type), is_v2_like(derived)) {
                                    (true, false) => {
                                        if stale.len() < 3 {
                                            stale.push((p.address, p.dex_type, derived));
                                        }
                                        p.dex_type = derived;
                                        p.derivation_version = Some(POOL_DERIVATION_VERSION);
                                        match cache.put_discovered_pool(&p) {
                                            Ok(()) => health.repaired += 1,
                                            Err(e) => tracing::warn!(
                                                pool = %p.address,
                                                "failed to persist repaired pool derivation: {e}"
                                            ),
                                        }
                                    }
                                    (false, true) => health.downgrades_refused += 1,
                                    _ => {}
                                }
                            }
                        }
                    }
                    tokens.insert(p.address, (p.token0, p.token1));
                    if is_skim_eligible(&p) {
                        v2_like.insert(p.address);
                    }
                    if p.factory.is_some_and(is_epoch_venue_factory) {
                        epoch_venue.insert(p.address);
                    }
                    if is_v2_like(p.dex_type) && p.factory.is_none() {
                        health.unverified_v2_like += 1;
                    }
                }
            }
            Err(e) => tracing::warn!("pool registry load failed: {e}"),
        },
        Err(e) => tracing::warn!("pool registry open failed: {e}"),
    }
    if health.repaired > 0 {
        let sample: Vec<String> = stale
            .iter()
            .map(|(address, stored, derived)| {
                format!("{address:#x}: stored {stored} != derived {derived}")
            })
            .collect();
        tracing::warn!(
            version = POOL_DERIVATION_VERSION,
            stale = health.stale_derivations,
            repaired = health.repaired,
            unverified_v2_like = health.unverified_v2_like,
            sample = ?sample,
            "repaired stale pool rows that disagreed with current DEX inference"
        );
    } else if health.stale_derivations > 0 {
        tracing::debug!(
            version = POOL_DERIVATION_VERSION,
            stale = health.stale_derivations,
            downgrades_refused = health.downgrades_refused,
            "pool registry family drift is ignored: it does not affect skim eligibility"
        );
    }
    if health.downgrades_refused > 0 {
        tracing::warn!(
            version = POOL_DERIVATION_VERSION,
            refused = health.downgrades_refused,
            "refused to re-derive pools into a V2-like family from a generic dex_name (would enable skim)"
        );
    } else if health.unverified_v2_like > 0 {
        tracing::debug!(
            version = POOL_DERIVATION_VERSION,
            unverified_v2_like = health.unverified_v2_like,
            "V2-like registry rows without a verified factory (skim-ineligible)"
        );
    }
    PoolRegistry {
        tokens,
        v2_like,
        epoch_venue,
        health,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry row for identity/eligibility tests. `factory` stands for
    /// provenance written by a configured factory scan, not runtime proof.
    fn identity_pool(dex_type: DexType, factory: Option<Address>, name: Option<&str>) -> PoolInfo {
        PoolInfo {
            address: Address::from([7u8; 20]),
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            dex_type,
            factory,
            dex_name: name.map(std::sync::Arc::from),
            ..Default::default()
        }
    }

    /// 3.1.3: the full identity matrix. Only a positively identified V2-like
    /// family with factory provenance is skim-eligible; empty/ambiguous names
    /// strict-infer to no family, so they fail closed as `Unknown`.
    #[test]
    fn skim_eligibility_identity_matrix_is_fail_closed() {
        let factory = Address::from([4u8; 20]);

        // UniswapV2 + factory provenance -> eligible.
        assert!(is_skim_eligible(&identity_pool(
            DexType::UniswapV2,
            Some(factory),
            Some("UniswapV2")
        )));
        // LB + factory provenance -> not a V2 skim venue.
        assert!(!is_skim_eligible(&identity_pool(
            DexType::TraderJoeLB,
            Some(factory),
            Some("pharaoh")
        )));
        // Unknown family + any non-null factory -> not eligible.
        assert!(!is_skim_eligible(&identity_pool(
            DexType::Unknown,
            Some(factory),
            None
        )));
        // CL family + any non-null factory -> not eligible.
        assert!(!is_skim_eligible(&identity_pool(
            DexType::UniswapV3,
            Some(factory),
            Some("uniswap-v3")
        )));

        // Empty name: strict inference yields no family -> effective Unknown.
        assert_eq!(infer_dex_type_strict(Some(""), &[]), None);
        assert!(!is_skim_eligible(&identity_pool(
            infer_dex_type_strict(Some(""), &[]).unwrap_or(DexType::Unknown),
            Some(factory),
            Some("")
        )));
        // Ambiguous name: strict inference yields no family -> Unknown.
        assert_eq!(infer_dex_type_strict(Some("unknown-amm"), &[]), None);
        assert!(!is_skim_eligible(&identity_pool(
            infer_dex_type_strict(Some("unknown-amm"), &[]).unwrap_or(DexType::Unknown),
            Some(factory),
            Some("unknown-amm")
        )));
    }

    /// 3.1.3: a stored V2 label with factory provenance is explicitly tested,
    /// not silently trusted. Empty/ambiguous metadata carries no evidence to
    /// demote the row, so it stays skim-eligible; a name that positively
    /// identifies a different family is repaired by the loader (V2 -> non-V2).
    /// Factory provenance is not a claim of runtime verification.
    #[test]
    fn v2_label_with_factory_provenance_is_explicitly_tested() {
        let factory = Address::from([4u8; 20]);
        for name in [None, Some(""), Some("unknown-amm")] {
            let pool = identity_pool(DexType::UniswapV2, Some(factory), name);
            assert_eq!(rederived_dex_type(&pool), None, "name {name:?}");
            assert!(
                is_skim_eligible(&pool),
                "no counter-evidence to demote the stored V2 family: {name:?}"
            );
        }

        // `pharaoh` positively identifies LB; the loader repairs the stored
        // V2 row, which removes skim eligibility.
        let pharaoh = identity_pool(DexType::UniswapV2, Some(factory), Some("pharaoh"));
        assert_eq!(rederived_dex_type(&pharaoh), Some(DexType::TraderJoeLB));
        assert!(!is_v2_like(DexType::TraderJoeLB));
    }

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

    #[test]
    fn stale_derivations_are_detected_not_trusted() {
        let mut pool = PoolInfo {
            address: Address::from([9u8; 20]),
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            dex_type: DexType::UniswapV2,
            factory: None,
            ..Default::default()
        };
        // The historical false-positive shape: a Pharaoh DLMM pool stored as
        // plain UniswapV2 with no factory.
        pool.dex_name = Some(std::sync::Arc::from("pharaoh"));
        assert_eq!(rederived_dex_type(&pool), Some(DexType::TraderJoeLB));
        assert_ne!(
            is_v2_like(pool.dex_type),
            is_v2_like(DexType::TraderJoeLB),
            "the stale row changes skim eligibility"
        );

        // A correctly derived row is not flagged.
        pool.dex_name = Some(std::sync::Arc::from("UniswapV2"));
        assert_eq!(rederived_dex_type(&pool), Some(DexType::UniswapV2));
        assert_eq!(pool.dex_type, DexType::UniswapV2);

        // No name means nothing to re-derive.
        pool.dex_name = None;
        assert_eq!(rederived_dex_type(&pool), None);

        // A name that does not round-trip must not be re-derived at all —
        // otherwise the row would be downgraded to a V2 skim venue.
        pool.dex_name = Some(std::sync::Arc::from("Trader Joe LB"));
        assert_eq!(rederived_dex_type(&pool), None);
    }

    /// Fail-closed default: an unclassified pool is never V2-like, so it can
    /// never become skim-eligible even if it carries a factory.
    #[test]
    fn unknown_family_is_never_skim_eligible() {
        assert!(!is_v2_like(DexType::Unknown));
        let pool = PoolInfo {
            address: Address::from([5u8; 20]),
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            dex_type: DexType::Unknown,
            factory: Some(Address::from([4u8; 20])),
            ..Default::default()
        };
        assert!(!is_skim_eligible(&pool));
        assert_eq!(PoolInfo::default().dex_type, DexType::Unknown);
    }

    /// End-to-end self-heal on a synthetic copy of the historical stale row:
    /// the registry must repair the persisted row, not just filter it in RAM.
    #[test]
    fn stale_row_is_repaired_and_stamped() {
        let dir = std::env::temp_dir().join(format!(
            "mev_scout_phase1_repair_{}_{}",
            std::process::id(),
            POOL_DERIVATION_VERSION
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("registry.sqlite");

        let stale = PoolInfo {
            address: Address::repeat_byte(0x8a),
            token0: Address::repeat_byte(0x11),
            token1: Address::repeat_byte(0x22),
            fee: 30,
            dex_type: DexType::UniswapV2,
            dex_name: Some(std::sync::Arc::from("pharaoh")),
            factory: None,
            derivation_version: None,
            ..Default::default()
        };
        {
            let store = SqliteStore::open(&db).unwrap();
            store.put_discovered_pool(&stale).unwrap();
            // A second row with the same generic name but a CL family must
            // NOT be downgraded — both families are skim-ineligible, and the
            // on-chain V3 classification is more specific than the label.
            store
                .put_discovered_pool(&PoolInfo {
                    address: Address::repeat_byte(0x8b),
                    token0: Address::repeat_byte(0x33),
                    token1: Address::repeat_byte(0x44),
                    fee: 3000,
                    dex_type: DexType::UniswapV3,
                    dex_name: Some(std::sync::Arc::from("pharaoh")),
                    ..Default::default()
                })
                .unwrap();
            // A bare "uniswap" label on a V3 row would be inferred as classic
            // V2; that must be refused (it would enable skim), not repaired.
            store
                .put_discovered_pool(&PoolInfo {
                    address: Address::repeat_byte(0x8c),
                    token0: Address::repeat_byte(0x55),
                    token1: Address::repeat_byte(0x66),
                    fee: 3000,
                    dex_type: DexType::UniswapV3,
                    dex_name: Some(std::sync::Arc::from("uniswap")),
                    ..Default::default()
                })
                .unwrap();
        }

        let mut config = Config::default();
        config.output.db_path = db.to_string_lossy().into_owned();
        let registry = load_pool_registry(&config, &ChainName::Avalanche);
        assert_eq!(registry.health.stale_derivations, 3);
        assert_eq!(registry.health.repaired, 1);
        assert_eq!(registry.health.downgrades_refused, 1);
        assert!(!registry.v2_like.contains(&stale.address));
        assert_eq!(
            registry.tokens.get(&stale.address).map(|t| t.0),
            Some(stale.token0)
        );

        {
            let store = SqliteStore::open(&db).unwrap();
            let repaired = store
                .get_discovered_pool(&stale.address)
                .unwrap()
                .expect("row must still exist after repair");
            assert_eq!(repaired.dex_type, DexType::TraderJoeLB);
            assert_eq!(repaired.derivation_version, Some(POOL_DERIVATION_VERSION));

            let generic_v3 = store
                .get_discovered_pool(&Address::repeat_byte(0x8b))
                .unwrap()
                .expect("CL row must still exist");
            assert_eq!(generic_v3.dex_type, DexType::UniswapV3);
            assert_eq!(generic_v3.derivation_version, None);

            let refused = store
                .get_discovered_pool(&Address::repeat_byte(0x8c))
                .unwrap()
                .expect("CL row must still exist");
            assert_eq!(refused.dex_type, DexType::UniswapV3);
        }
        // A second load trusts the repaired row; the harmless family drifts
        // are re-counted but never rewritten. The refused downgrade persists.
        let second = load_pool_registry(&config, &ChainName::Avalanche);
        assert_eq!(second.health.repaired, 0);
        assert_eq!(second.health.stale_derivations, 2);
        assert_eq!(second.health.downgrades_refused, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
