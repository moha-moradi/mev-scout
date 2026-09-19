//! Pre-cache block data without running strategies.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation;
use crate::config::Config;
use crate::fetch::Fetcher;
use crate::progress::{JobProgress, ProgressEvent};
use crate::resolver::RangeResolver;
use crate::utils::epoch_secs;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct FetchOpts {
    pub batch_rpc: bool,
    pub no_sig_resolve: bool,
}

pub struct FetchOutcome {
    pub run_id: String,
    pub total_blocks: u64,
    pub fetched: u64,
    pub cached: u64,
    pub elapsed_secs: f64,
    pub refetched: u64,
}

pub async fn job_fetch(
    config: &Config,
    opts: &FetchOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<FetchOutcome> {
    let (chain_name, _chain_config) =
        validation::resolve_chain(config).context("failed to resolve chain")?;

    let setup = init_rpc(config, chain_name, true)
        .await
        .context("failed to initialize RPC client")?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    progress.log(&rpc.provider_summary().await);

    let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;

    let range_mode = validation::resolve_block_range(
        config.days,
        config.blocks,
        config.block,
        config.from_block,
        config.to_block,
    )
    .context("invalid block range — pass --days, --blocks, --block, or --from-block/--to-block")?;
    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&range_mode).await?;

    let run_id = format!("run_{}", epoch_secs());
    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        resolved_at: epoch_secs(),
        range_mode: resolved.mode_string(),
        strategies: vec![],
        flash_loan_provider: String::new(),
    };
    cache.put_manifest(&manifest)?;

    progress.log(&format!("Run ID: {run_id}"));
    progress.log(&resolved.summary());
    progress.emit(ProgressEvent::stage("fetch"));

    let mut fetcher = Fetcher::new(rpc, cache);
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(opts.batch_rpc);
    let bc = config.effective_block_concurrency(chain_name, &provider_configs);
    fetcher = fetcher.with_block_concurrency(bc);
    if !opts.no_sig_resolve {
        match crate::sigs::ensure_signature_db(None).await {
            Ok(sig_db_path) => match crate::sigs::SignatureResolver::new(&sig_db_path) {
                Ok(resolver) => {
                    fetcher = fetcher.with_sig_resolver(resolver);
                    progress.log("Signature resolution enabled");
                }
                Err(e) => progress.log(&format!(
                    "Failed to load signature DB: {e} — continuing without sig resolution"
                )),
            },
            Err(e) => progress.log(&format!(
                "Failed to ensure signature DB: {e} — continuing without sig resolution"
            )),
        }
    } else {
        progress.log("Signature resolution disabled (--no-sig-resolve)");
    }

    let fetch_total = resolved.block_count;
    let fetch_done = Arc::new(AtomicU64::new(0));
    let tick = move || {
        if progress.cancelled() {
            return false;
        }
        let d = fetch_done.fetch_add(1, Ordering::Relaxed) + 1;
        progress.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(d),
            total: Some(fetch_total),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        true
    };
    let summary = fetcher.fetch_range(&resolved, Some(&tick)).await?;

    progress.log("");
    progress.log("Fetch complete:");
    progress.log(&format!("  Total blocks: {}", summary.total_blocks));
    progress.log(&format!("  Fetched:      {}", summary.fetched));
    progress.log(&format!("  Cached:       {}", summary.cached));
    progress.log(&format!("  Elapsed:      {:.2}s", summary.elapsed_secs));

    let mut refetched = 0u64;
    if !summary.missing_after_fetch.is_empty() {
        progress.log(&format!(
            "  Missing:      {} blocks — auto-refetching...",
            summary.missing_after_fetch.len()
        ));
        refetched = fetcher
            .auto_refetch_gaps(&summary.missing_after_fetch)
            .await?;
        progress.log(&format!("  Refetched:    {refetched}"));
    }

    Ok(FetchOutcome {
        run_id,
        total_blocks: summary.total_blocks,
        fetched: summary.fetched,
        cached: summary.cached,
        elapsed_secs: summary.elapsed_secs,
        refetched,
    })
}
