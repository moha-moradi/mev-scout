use std::time::{SystemTime, UNIX_EPOCH};

use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::FetchArgs;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub async fn cmd_fetch(config: &Config, args: &FetchArgs) -> anyhow::Result<()> {
    let chain_name: ChainName = match args.chain_args.chain.parse() {
        Ok(c) => c,
        Err(e) => anyhow::bail!("Error: {e}"),
    };

    let provider_configs = config.effective_provider_configs(chain_name)?;
    let chain_id = chain_name.chain_id();
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(config.rps_limit)).collect::<Vec<_>>()).await;
    rpc.check_connection(chain_id).await?;
    tracing::info!("{}", rpc.provider_summary().await);

    let cache = SqliteStore::open(&config.effective_db_path(&chain_name), chain_id)?;

    let range_mode = match validation::resolve_block_range(
        args.block_range.days,
        args.block_range.blocks,
        args.block_range.block,
        args.block_range.from_block,
        args.block_range.to_block,
    ) {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{e}"),
    };

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&range_mode).await?;

    let run_id = format!(
        "run_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock went backwards")
            .as_secs()
    );

    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        resolved_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock went backwards")
            .as_secs(),
        range_mode: resolved.mode_string(),
        strategies: vec![],
        flash_loan_provider: String::new(),
    };
    cache.put_manifest(&manifest)?;

    println!("Run ID: {}", run_id);
    println!("{}", resolved.summary());
    println!();

    let mut fetcher = Fetcher::new(rpc, cache);
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(args.batch_rpc);
    let bc = config.effective_block_concurrency(&provider_configs);
    fetcher = fetcher.with_block_concurrency(bc);
    if !args.no_sig_resolve {
        match mev_scout_core::sigs::ensure_signature_db(None).await {
            Ok(sig_db_path) => {
                match mev_scout_core::sigs::SignatureResolver::new(&sig_db_path) {
                    Ok(resolver) => {
                        fetcher = fetcher.with_sig_resolver(resolver);
                        tracing::info!("Signature resolution enabled");
                    }
                    Err(e) => tracing::warn!("Failed to load signature DB: {e} — continuing without sig resolution"),
                }
            }
            Err(e) => tracing::warn!("Failed to ensure signature DB: {e} — continuing without sig resolution"),
        }
    } else {
        tracing::info!("Signature resolution disabled (--no-sig-resolve)");
    }

    let pb = ProgressBar::new(resolved.block_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
            .progress_chars("=> "),
    );

    let tick = || pb.inc(1);
    let summary = fetcher.fetch_range(&resolved, Some(&tick)).await?;
    pb.finish_and_clear();

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
