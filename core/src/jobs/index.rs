use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::config::validation;
use crate::config::Config;
use crate::explorer::ingest::{backfill_range, run_live, safe_head, IngestConfig};
use crate::explorer::store::ExplorerStore;
use crate::progress::JobProgress;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct IndexOpts {
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub days: Option<u64>,
    pub live: bool,
    pub duration: Option<String>,
}

pub struct IndexOutcome {
    pub blocks_indexed: u64,
    pub ops: u64,
    pub elapsed: Duration,
    pub live: bool,
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

    if opts.live {
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
            .map(|d| {
                humantime::parse_duration(d).with_context(|| format!("invalid duration '{d}'"))
            })
            .transpose()?;
        let stop_deadline = stop.clone();
        let deadline_poll = async move {
            if let Some(dur) = deadline_dur {
                tokio::time::sleep(dur).await;
                stop_deadline.store(true, Ordering::Relaxed);
            }
        };

        let t0 = Instant::now();
        let (_, _, indexed) = tokio::join!(
            cancel_poll,
            deadline_poll,
            run_live(
                &setup.rpc,
                &store,
                &cfg,
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
        return Ok(IndexOutcome {
            blocks_indexed: indexed,
            ops: store.op_count_since(0)?,
            elapsed,
            live: true,
        });
    }

    let head = safe_head(&setup.rpc, &cfg).await?;
    let (from, to) = match (opts.from, opts.to) {
        (Some(fm), Some(t)) => (fm, t),
        (None, None) => {
            let days = opts.days.unwrap_or(30);
            let blocks_per_day = crate::chain::timing::blocks_per_day(chain);
            let n = days.saturating_mul(blocks_per_day).max(1);
            (head.saturating_sub(n), head)
        }
        _ => anyhow::bail!("--from and --to must be used together (or use --days)"),
    };
    if to > head {
        anyhow::bail!("--to {to} is beyond safe head {head}");
    }

    progress.log(&format!(
        "Indexing blocks {from}-{to} ({} blocks) — {chain}",
        to - from + 1
    ));
    let t0 = Instant::now();
    let (blocks_done, ops) = backfill_range(
        &setup.rpc,
        &store,
        &cfg,
        from,
        to,
        config.explorer.checkpoint_every,
    )
    .await?;
    let elapsed = t0.elapsed();
    progress.log(&format!(
        "Indexed {blocks_done} blocks, {ops} ops in {:.1}s",
        elapsed.as_secs_f64(),
    ));

    Ok(IndexOutcome {
        blocks_indexed: blocks_done,
        ops,
        elapsed,
        live: false,
    })
}
