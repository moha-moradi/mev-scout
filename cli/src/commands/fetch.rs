use anyhow::Context;
use mev_scout_core::utils::epoch_secs;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cli::FetchArgs;
use crate::job_progress::{JobProgress, ProgressEvent};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::validation::RangeSpec;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::resolver::RangeResolver;

pub async fn cmd_fetch(
    config: &Config,
    args: &FetchArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let (chain_name, _chain_config) =
        validation::resolve_chain(config).context("failed to resolve chain")?;

    let setup = init_rpc(config, chain_name, true)
        .await
        .context("failed to initialize RPC client")?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    tracing::info!("{}", rpc.provider_summary().await);

    let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;

    let range_mode = RangeSpec::try_from(&args.block_range)
        .context("invalid block range")?
        .resolve();

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
    progress.log("");

    let mut fetcher = Fetcher::new(rpc, cache);
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(args.batch_rpc);
    let bc = config.effective_block_concurrency(&provider_configs);
    fetcher = fetcher.with_block_concurrency(bc);
    if !args.no_sig_resolve {
        match mev_scout_core::sigs::ensure_signature_db(None).await {
            Ok(sig_db_path) => match mev_scout_core::sigs::SignatureResolver::new(&sig_db_path) {
                Ok(resolver) => {
                    fetcher = fetcher.with_sig_resolver(resolver);
                    tracing::info!("Signature resolution enabled");
                }
                Err(e) => tracing::warn!(
                    "Failed to load signature DB: {e} — continuing without sig resolution"
                ),
            },
            Err(e) => tracing::warn!(
                "Failed to ensure signature DB: {e} — continuing without sig resolution"
            ),
        }
    } else {
        tracing::info!("Signature resolution disabled (--no-sig-resolve)");
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

    println!();
    println!("Fetch complete:");
    println!("  Total blocks: {}", summary.total_blocks);
    println!("  Fetched:      {}", summary.fetched);
    println!("  Cached:       {}", summary.cached);
    println!("  Elapsed:      {:.2}s", summary.elapsed_secs);
    if summary.phase_db_ms > 0.0 {
        println!("  Phase timing: DB lookup {:.1}ms | distribute {:.1}ms | fetch {:.1}ms | integrity {:.1}ms | flush {:.1}ms",
            summary.phase_db_ms,
            summary.phase_distribute_ms,
            summary.elapsed_secs * 1000.0 - summary.phase_db_ms - summary.phase_distribute_ms - summary.phase_integrity_ms - summary.phase_flush_ms,
            summary.phase_integrity_ms,
            summary.phase_flush_ms,
        );
    }

    if !summary.missing_after_fetch.is_empty() {
        println!(
            "  Missing:      {} blocks — auto-refetching...",
            summary.missing_after_fetch.len()
        );
        let refetched = fetcher
            .auto_refetch_gaps(&summary.missing_after_fetch)
            .await?;
        println!("  Refetched:    {}", refetched);
    }

    Ok(())
}
