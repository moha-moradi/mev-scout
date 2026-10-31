//! Live/tip explorer indexing for tag coverage.
//!
//! Gated: the test is `#[ignore]`d, so a plain `cargo test` run skips it. Set
//! `MEV_SCOUT_COVERAGE=1` and select it explicitly:
//! ```text
//! cargo test -p mev-scout-core --test explorer_tag_coverage -- --ignored --nocapture
//! ```
//! Optional: `MEV_SCOUT_COVERAGE_DURATION=15m`, `MEV_SCOUT_COVERAGE_CONFIG=mev-scout.coverage.toml`,
//! and `MEV_SCOUT_COVERAGE_STRICT=1` to fail on incomplete tag coverage instead
//! of printing a warning.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mev_scout_core::config::Config;
use mev_scout_core::explorer::ingest::{run_live, IngestConfig, PoolViews};
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::jobs::{init_rpc, load_pool_registry};
use mev_scout_core::types::ChainName;

/// Single source of the classifier's tag vocabulary, shared with
/// `scripts/tag_coverage.py`.
fn expected_tags() -> Vec<String> {
    serde_json::from_str(include_str!("../data/explorer_tags.json"))
        .unwrap_or_else(|e| panic!("core/data/explorer_tags.json: {e}"))
}

/// Morpho bad-debt is synthetic-only on Avalanche — not required for AVAX coverage.
const AVAX_SKIP: &[&str] = &["bad_debt_liq"];

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live RPC — set MEV_SCOUT_COVERAGE=1"]
async fn tip_loop_tag_coverage() -> anyhow::Result<()> {
    if std::env::var("MEV_SCOUT_COVERAGE").ok().as_deref() != Some("1") {
        eprintln!("skip: set MEV_SCOUT_COVERAGE=1 to run");
        return Ok(());
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let cfg_path = std::env::var("MEV_SCOUT_COVERAGE_CONFIG").unwrap_or_else(|_| {
        // Prefer workspace-root config when `cargo test -p` cwd is `core/`.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("mev-scout.coverage.toml"))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("mev-scout.coverage.toml"));
        root.to_string_lossy().into_owned()
    });
    let duration = parse_duration_env(
        &std::env::var("MEV_SCOUT_COVERAGE_DURATION").unwrap_or_else(|_| "10m".into()),
    )?;

    eprintln!("config={cfg_path}");
    let config = Config::load(&cfg_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = ChainName::Avalanche;
    let setup = init_rpc(&config, chain, true).await?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    let (_, chain_cfg) = mev_scout_core::config::validation::resolve_chain(&config)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut ingest = IngestConfig::from_chain(chain, &chain_cfg);
    ingest.confirmations = config.explorer.confirmations;
    ingest.rpc_batch_blocks = config.effective_rpc(chain).rpc_batch_blocks;

    let registry = load_pool_registry(&config, &chain);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let deadline = tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        stop2.store(true, Ordering::Relaxed);
    });

    eprintln!("Live coverage index — {chain} for {duration:?}");
    let indexed = run_live(
        &setup.rpc,
        &store,
        &ingest,
        PoolViews {
            tokens: &registry.tokens,
            v2_like: &registry.v2_like,
            epoch_venue: &registry.epoch_venue,
        },
        config.explorer.poll_interval_ms,
        stop,
        &mev_scout_core::progress::NoopProgress,
    )
    .await?;
    let _ = deadline.await;
    eprintln!(
        "indexed {indexed} tip blocks; ops={}",
        store.op_count_since(0)?
    );

    let tags = collect_tags(&store)?;
    let skip: HashSet<&str> = AVAX_SKIP.iter().copied().collect();
    let expected = expected_tags();
    let missing: Vec<&str> = expected
        .iter()
        .map(String::as_str)
        .filter(|t| !tags.contains(*t) && !skip.contains(t))
        .collect();
    let mut present: Vec<_> = tags.iter().cloned().collect();
    present.sort();
    eprintln!("tags present ({}): {present:?}", present.len());
    eprintln!("missing: {missing:?}");
    if std::env::var("MEV_SCOUT_COVERAGE_STRICT").ok().as_deref() == Some("1") {
        anyhow::ensure!(
            missing.is_empty(),
            "tag coverage incomplete after {duration:?}: missing {missing:?}"
        );
    } else if !missing.is_empty() {
        eprintln!(
            "WARN incomplete coverage (set MEV_SCOUT_COVERAGE_STRICT=1 to fail): {missing:?}"
        );
    }
    anyhow::ensure!(
        indexed > 0 || store.op_count_since(0)? > 0,
        "no blocks indexed"
    );
    Ok(())
}

fn parse_duration_env(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('s') {
        return Ok(Duration::from_secs(num.parse()?));
    }
    if let Some(num) = s.strip_suffix('m') {
        return Ok(Duration::from_secs(num.parse::<u64>()? * 60));
    }
    if let Some(num) = s.strip_suffix('h') {
        return Ok(Duration::from_secs(num.parse::<u64>()? * 3600));
    }
    Ok(Duration::from_secs(s.parse()?))
}

fn collect_tags(store: &ExplorerStore) -> anyhow::Result<HashSet<String>> {
    let mut out = HashSet::new();
    for op in store.ops_in_range(0, u64::MAX, &[])? {
        let Some(dj) = op.details_json.as_deref() else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(dj).unwrap_or(serde_json::json!({}));
        if let Some(arr) = v.get("tags").and_then(|t| t.as_array()) {
            for t in arr {
                if let Some(s) = t.as_str() {
                    out.insert(s.to_string());
                }
            }
        }
    }
    Ok(out)
}
