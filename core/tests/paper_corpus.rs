//! Paper ↔ executed-profit corpus (MEV-VERIFICATION §B.3).
//!
//! Pipeline: `job_run` (detector) over a fixed historical window → pure
//! [`LedgerPolicy::apply`] (the same arithmetic `paper run` performs; wallet
//! delta = `expected_profit − gas_cost_wei`) → re-execute each fill's real
//! anchored tx through revm ([`BlockReplayer::what_if_real_txs`], the §B.1
//! executor) → reconcile via [`paper_vs_executed`] and assert **derived facts**
//! — never exact profit amounts:
//!
//! - the ledger accepted at least one fill over the window,
//! - every fill with an executed counterpart actually executed (status=true,
//!   gas_used>0) — a paper fill whose anchor tx reverted is decisive evidence,
//! - verifiable pass-rate `pass / (pass + fail)` ≥ 0.5 (fills with no executed
//!   counterpart — mempool-only, unanchored, or outside the replayed block
//!   range — surface as `Unverifiable`, degraded coverage rather than a fail).
//!
//! Why re-execution rather than a stored tx trace: the executed ground truth is
//! produced locally by revm against the same state the backtest replays, so the
//! comparison never leans on a tracing RPC result or the classifier's price
//! attribution. The window is the same Avalanche seed slice as `mev_corpus.rs`
//! (95681722..=95682322), gated identically (`MEV_SCOUT_E2E=1` + `RPC_URL`).
//!
//! ## Seeding
//! `MEV_SCOUT_RECORD=1` runs the window and prints observed reconciliation
//! facts (pass/fail/unverifiable counts + fail-reason histogram) without
//! asserting — use it to sanity-check the window before relaxing/raising the
//! floor.
mod common;
use common::rpc_url;

use std::collections::HashMap;

use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::jobs::{job_run, RunOpts};
use mev_scout_core::paper::{
    paper_vs_executed, LedgerPolicy, LedgerResult, PaperFill, ReconReport,
};
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::replay::{BlockReplayer, ExecutedNet, ExecutedNetMap, WhatIfRun};
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

const CHAIN: ChainName = ChainName::Avalanche;
/// Same seed window as `mev_corpus.rs` (§D): 630 realized `arb_atomic` ops and
/// dense block 95682057.
const FROM_BLOCK: u64 = 95681722;
const TO_BLOCK: u64 = 95682322;
/// Floor on `pass / (pass + fail)` over fills with an executed counterpart.
const PASS_RATE_FLOOR: f64 = 0.5;

// 0.50 USD in native wei (matches `explorer.mev_error_usd_tol`); only used for
// the paper≈0 corner case of `paper_vs_executed`.
const ABS_WEI_TOL: i128 = (0.50 * 1e18) as i128;

fn repo_cache(name: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("cache")
        .join(name);
    p.exists().then_some(p)
}

/// Seed the temp block cache's `pool_info` registry from the pools the
/// realizer's seed actually swapped on (repo seed block caches hold no registry
/// of their own). Mirrors `mev_corpus.rs`.
fn seed_pool_registry(blocks: &SqliteStore, explorer: &ExplorerStore) {
    let Ok(rows) = explorer.pools_from_swaps() else {
        eprintln!("WARN: no swap-derived pools to seed registry from");
        return;
    };
    let mut count = 0usize;
    for (pool, amm, token_in, token_out) in rows {
        let (Ok(address), Ok(tin), Ok(tout)) = (
            pool.parse::<alloy::primitives::Address>(),
            token_in.parse::<alloy::primitives::Address>(),
            token_out.parse::<alloy::primitives::Address>(),
        ) else {
            continue;
        };
        let dex_type = match amm.as_deref() {
            Some("v2") => DexType::UniswapV2,
            Some("v3") => DexType::UniswapV3,
            _ => continue,
        };
        if tin == tout {
            continue;
        }
        let (token0, token1) = if tin < tout { (tin, tout) } else { (tout, tin) };
        let info = PoolInfo {
            address,
            token0,
            token1,
            dex_type,
            ..Default::default()
        };
        if blocks.put_discovered_pool(&info).is_ok() {
            count += 1;
        }
    }
    eprintln!("seeded pool registry with {count} pools from explorer swaps");
}

/// Temp workspace: block-cache copy (+ pool registry seed) and explorer copy
/// (swap/pool seed reference). Repo caches are never written.
struct Workspace {
    blocks: std::path::PathBuf,
    explorer_db: std::path::PathBuf,
    _dir: std::path::PathBuf,
}

impl Workspace {
    fn new(chain: ChainName) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("paper_corpus_{chain}_{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;

        let blocks = dir.join(format!("{chain}-mev-scout.sqlite"));
        if let Some(seed) = repo_cache(&format!("{chain}-mev-scout.sqlite")) {
            let _ = std::fs::copy(&seed, &blocks);
        }

        let explorer_db = dir.join(format!("explorer-{chain}.sqlite"));
        if let Some(seed) = repo_cache(&format!("explorer-{chain}.sqlite")) {
            let _ = std::fs::copy(&seed, &explorer_db);
        }
        let explorer = ExplorerStore::open(&explorer_db)
            .map_err(|e| std::io::Error::other(format!("open explorer copy: {e}")))?;

        if let Ok(blocks_store) = SqliteStore::open(&blocks) {
            seed_pool_registry(&blocks_store, &explorer);
        }

        Ok(Workspace {
            blocks,
            explorer_db,
            _dir: dir,
        })
    }
}

/// Runtime config: chain + caller RPC, block fetch redirected to the temp
/// workspace (derived `./cache/…` fallbacks would otherwise mutate repo).
#[allow(clippy::field_reassign_with_default)]
fn corpus_config(rpc_url: &str, ws: &Workspace) -> Config {
    let mut config = Config::default();
    config.chain = CHAIN;
    config.rpc.rpc_url = Some(rpc_url.to_string());
    config.rpc.rps_limit = 5.0;
    config.output.db_path = ws.blocks.to_string_lossy().into_owned();
    config.explorer.db_path = ws.explorer_db.to_string_lossy().into_owned();
    config
}

/// Re-execute every tx-anchored fill's real on-chain tx through revm and key
/// the executed nets by `canonical_id` (the §B.2 join key). Mempool-only /
/// unanchored fills never enter the executed map and reconcile as
/// `Unverifiable`.
fn build_executed(
    replayer: &BlockReplayer,
    ledger: &LedgerResult,
) -> anyhow::Result<ExecutedNetMap> {
    let mut by_block: std::collections::BTreeMap<u64, Vec<&PaperFill>> =
        std::collections::BTreeMap::new();
    for fill in &ledger.fills {
        if fill.tx_index.is_some() {
            by_block.entry(fill.block_number).or_default().push(fill);
        }
    }

    let mut executed = ExecutedNetMap::new();
    for (block, fills) in by_block {
        let indices: Vec<usize> = fills.iter().map(|f| f.tx_index.unwrap()).collect();
        let runs = replayer.what_if_real_txs(block, &indices)?;
        let by_tx: HashMap<usize, WhatIfRun> = runs.into_iter().collect();
        for fill in fills {
            let Some(exec) = by_tx
                .get(&fill.tx_index.unwrap())
                .map(ExecutedNet::from_run)
            else {
                continue;
            };
            if let Some(cid) = &fill.canonical_id {
                executed.insert(cid.clone(), exec);
            }
        }
    }
    Ok(executed)
}

fn summarize(report: &ReconReport) -> Vec<String> {
    let mut lines = vec![format!(
        "  fills {} — {} pass / {} fail / {} unverifiable (rate {})",
        report.fill_count(),
        report.pass,
        report.fail,
        report.unverifiable,
        report
            .verifiable_pass_rate()
            .map(|r| format!("{r:.2}"))
            .unwrap_or_else(|| "n/a".into()),
    )];
    let mut hist: Vec<_> = report
        .fills
        .iter()
        .flat_map(|f| f.verdict.reason())
        .fold(HashMap::<&str, usize>::new(), |mut m, r| {
            let bucket = r
                .split(|c: char| !c.is_ascii_alphanumeric())
                .next()
                .unwrap_or("other");
            *m.entry(bucket).or_default() += 1;
            m
        })
        .into_iter()
        .collect();
    hist.sort_by_key(|b| std::cmp::Reverse(b.1));
    for (bucket, n) in hist.into_iter().take(5) {
        lines.push(format!("    {bucket}: {n}"));
    }
    lines
}

struct ReconOutcome {
    ledger_fills: usize,
    report: ReconReport,
}

async fn run_reconciliation(rpc_url: &str) -> anyhow::Result<ReconOutcome> {
    let ws = Workspace::new(CHAIN).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
    let config = corpus_config(rpc_url, &ws);
    let mut run = config.clone();
    run.from_block = Some(FROM_BLOCK);
    run.to_block = Some(TO_BLOCK);

    eprintln!(
        "{CHAIN}: detecting over {FROM_BLOCK}..={TO_BLOCK} (~{} blocks)",
        TO_BLOCK - FROM_BLOCK + 1
    );
    let outcome = job_run(
        &run,
        &RunOpts {
            // Providers rarely retain `eth_getLogs` deep enough for this
            // historical window; force a full fetch so detection does not
            // depend on the activity scan.
            fetch_relevant: false,
            ..Default::default()
        },
        &NoopProgress,
    )
    .await?;

    let ledger = LedgerPolicy::default().apply(&outcome.opportunities);
    eprintln!(
        "ledger: {} opps → {} fills ({} skipped)",
        outcome.opportunities.len(),
        ledger.fills_count(),
        ledger.skips_count()
    );

    let handle = tokio::runtime::Handle::current();
    let cache = SqliteStore::open(&ws.blocks)?;
    let rpc = RpcClient::new(rpc_url, CHAIN.chain_id())?;
    let replayer = BlockReplayer::new(handle, cache, rpc, CHAIN.chain_id());

    let executed = build_executed(&replayer, &ledger)?;
    eprintln!("what-if executed {} tx-anchored fills", executed.len());

    let report = paper_vs_executed(
        &ledger,
        &executed,
        config.explorer.mev_tolerance_pct,
        ABS_WEI_TOL,
    );
    Ok(ReconOutcome {
        ledger_fills: ledger.fills_count(),
        report,
    })
}

fn assert_derived_facts(outcome: &ReconOutcome) {
    let report = &outcome.report;

    assert!(
        outcome.ledger_fills >= 1,
        "ledger produced no fills over {FROM_BLOCK}..={TO_BLOCK} on {CHAIN} — the paper gate has \
         nothing to reconcile (peek with `MEV_SCOUT_RECORD=1`)"
    );

    for fill in report.fills.iter().filter(|f| f.executed_status.is_some()) {
        assert!(
            fill.executed_status == Some(true) && fill.executed_gas_used.unwrap_or(0) > 0,
            "fill {} ({}) anchor tx did NOT execute cleanly: status={:?} gas_used={:?} — paper \
             booked a net of {:?} wei that execution would not have produced",
            fill.canonical_id.as_deref().unwrap_or("?"),
            fill.tx_index.map(|i| format!("#{i}")).unwrap_or_default(),
            fill.executed_status,
            fill.executed_gas_used,
            fill.paper_net_wei,
        );
    }

    let rate = report
        .verifiable_pass_rate()
        .unwrap_or_else(|| panic!("no verifiable fills — every paper fill was Unverifiable"));
    assert!(
        rate >= PASS_RATE_FLOOR,
        "paper↔executed pass-rate {rate:.2} ({}/{}) below floor {PASS_RATE_FLOOR}",
        report.pass,
        report.pass + report.fail
    );
}

#[tokio::test]
async fn paper_corpus_reconciles_against_executed() {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return;
    };
    let _guard = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    if std::env::var("MEV_SCOUT_RECORD").is_ok_and(|v| v == "1") {
        match run_reconciliation(&rpc_url).await {
            Ok(o) => {
                println!("{CHAIN} {FROM_BLOCK}..={TO_BLOCK} (record mode, no assertions)");
                for line in summarize(&o.report) {
                    println!("{line}");
                }
            }
            Err(e) => eprintln!("Record: run failed for {CHAIN}: {e}"),
        }
        return;
    }

    match run_reconciliation(&rpc_url).await {
        Ok(o) => {
            for line in summarize(&o.report) {
                eprintln!("{line}");
            }
            assert_derived_facts(&o);
        }
        Err(e) => {
            eprintln!("Skipping: reconciliation failed for {CHAIN}: {e}");
        }
    }
}
