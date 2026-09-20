use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Context;

use crate::cache::SqliteStore;
use crate::config::validation;
use crate::config::Config;
use crate::explorer::ingest::{run_live, IngestConfig};
use crate::explorer::store::ExplorerStore;
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
    let pool_tokens = load_pool_registry(config, &chain);

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
            &pool_tokens,
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

/// Load the pool → (token0, token1) registry from the scanner cache (Phase 1.1).
/// Best-effort: an absent/locked cache yields an empty map and ingest degrades
/// to transfer-pairing resolution.
fn load_pool_registry(config: &Config, chain: &ChainName) -> HashMap<Address, (Address, Address)> {
    let mut map = HashMap::new();
    match SqliteStore::open(config.effective_db_path(chain)) {
        Ok(cache) => match cache.list_discovered_pools() {
            Ok(pools) => {
                for p in pools {
                    if !p.token0.is_zero() && !p.token1.is_zero() {
                        map.insert(p.address, (p.token0, p.token1));
                    }
                }
            }
            Err(e) => tracing::warn!("pool registry load failed: {e}"),
        },
        Err(e) => tracing::warn!("pool registry open failed: {e}"),
    }
    map
}
