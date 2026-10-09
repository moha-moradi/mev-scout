use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Context;

use crate::cache::SqliteStore;
use crate::config::validation;
use crate::config::Config;
use crate::dex_type::DexType;
use crate::explorer::ingest::{run_live, IngestConfig, PoolViews};
use crate::explorer::store::ExplorerStore;
use crate::pool::discovery::protocol_names::is_epoch_venue_factory;
use crate::progress::JobProgress;
use crate::types::ChainName;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct IndexOpts {
    pub duration: Option<String>,
}

pub struct IndexOutcome {
    pub blocks_indexed: u64,
    pub ops: u64,
    pub elapsed: Duration,
}

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

pub async fn job_index(
    config: &Config,
    opts: &IndexOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<IndexOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let setup = init_rpc(config, chain, true).await?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;

    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    cfg.arb_likely_parity = config.explorer.arb_likely_parity;

    // Phase 1.1: pool registry (pool → token0/token1) for swap-direction
    // resolution. Missing/empty registry degrades to transfer pairing.
    let registry = load_pool_registry(config, &chain);

    let t0 = Instant::now();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_cancel = stop.clone();
    let cancel_poll = async {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if progress.cancelled() {
                stop_cancel.store(true, Ordering::Relaxed);
                break;
            }
        }
    };
    let deadline_dur = opts
        .duration
        .as_deref()
        .map(|d| humantime::parse_duration(d).with_context(|| format!("invalid duration '{d}'")))
        .transpose()?;
    let stop_deadline = stop.clone();
    let deadline_poll = async move {
        if let Some(dur) = deadline_dur {
            tokio::time::sleep(dur).await;
            stop_deadline.store(true, Ordering::Relaxed);
        }
    };

    progress.log(&format!(
        "Live indexing — {chain} from current tip (head − {} confirmations; no historical catch-up)",
        cfg.confirmations
    ));
    let (_, _, indexed) = tokio::join!(
        cancel_poll,
        deadline_poll,
        run_live(
            &setup.rpc,
            &store,
            &cfg,
            PoolViews {
                tokens: &registry.tokens,
                v2_like: &registry.v2_like,
                epoch_venue: &registry.epoch_venue,
            },
            config.explorer.poll_interval_ms,
            stop,
        )
    );
    let indexed = indexed?;
    let elapsed = t0.elapsed();
    progress.log(&format!(
        "Live indexing done — {indexed} blocks indexed, {} ops total in {:.1}s",
        store.op_count_since(0)?,
        elapsed.as_secs_f64(),
    ));
    Ok(IndexOutcome {
        blocks_indexed: indexed,
        ops: store.op_count_since(0)?,
        elapsed,
    })
}

/// Load the pool → (token0, token1) registry and V2-like skim set from the
/// scanner cache (Phase 1.1). Best-effort: an absent/locked cache yields empty
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
                        if is_v2_like(p.dex_type) {
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
