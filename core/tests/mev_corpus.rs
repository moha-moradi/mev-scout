//! Detector real-block corpus (MEV-VERIFICATION §D).
//!
//! Runs the *backtest detector* over a fixed historical window from a live
//! RPC (pipeline: `Fetcher` → SQLite block cache → `PoolManager::init_from_rpc`
//! → `BacktestRunner::run_range`) and asserts **derived facts** — never exact
//! profit amounts:
//!
//! - kind present (`min_ops` floor) and searcher identity filters,
//! - detector-vs-realized verdict pass-rate band (default floor 0.5 per kind,
//!   user decision; `mev_verdict` over the tx-hash-only realization join).
//!
//! Realized reference data comes from the seed explorer DB
//! (`cache/explorer-{chain}.sqlite`), which the harness copies to a temp
//! location before the run so the repo caches are never mutated. The verdict
//! is wei-denominated: detector `expected_profit − gas_cost_wei` vs the
//! matched realized op's `trace_native_delta_wei` (fallback: `net_profit_usd`
//! converted via `prices` native row at the opp's block hour). Ops with no
//! verifiable realized side are excluded from the rate (Unverifiable ≠ fail).
//!
//! Gated like every other live-RPC test (`common::setup::rpc_url`):
//! `MEV_SCOUT_E2E=1` + `RPC_URL`, otherwise it skips with no network I/O.
//!
//! ## Seeding new cases
//!
//! Set `MEV_SCOUT_RECORD=1` alongside the E2E vars to run the union window of
//! the current cases and print observed per-kind / per-searcher / verdict
//! facts without asserting — record those into a `CorpusCase` below. The
//! sandwich/jit kinds are the current gap (explorer corpus has none in its
//! seed window either); a live detector run is how their windows get captured
//! here.
mod common;
use common::rpc_url;

use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::Address;

use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::jobs::{job_run, RunOpts};
use mev_scout_core::mev::{mev_verdict, MevVerdict};
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::types::{ChainName, MevOpportunity, Strategy};

/// One golden detector expectation.
struct CorpusCase {
    id: &'static str,
    chain: ChainName,
    from_block: u64,
    to_block: u64,
    kind: &'static str,
    min_ops: u64,
    searcher: Option<Address>,
    /// Minimum `pass / (pass + fail)` over realized-verifiable detector ops.
    /// `None` = skip the verdict band (kind/searcher/count still asserted).
    expected_verdict_rate: Option<f64>,
}

// Seed facts for the cheap cases derive from the same Avalanche window as
// `explorer_corpus.rs` (95681722..=95682322): 630 realized `arb_atomic` ops,
// dense block 95682057, and a liquidation at block 95682033. Detector floors
// are deliberately far below the realized counts — the corpus only requires
// the pipeline to actually find the kinds, not to match realizer throughput.
const CORPUS: &[CorpusCase] = &[
    CorpusCase {
        id: "av-det-arb-window",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 30,
        searcher: None,
        expected_verdict_rate: Some(0.5),
    },
    CorpusCase {
        id: "av-det-arb-dense-block",
        chain: ChainName::Avalanche,
        from_block: 95682057,
        to_block: 95682057,
        kind: "arb_atomic",
        min_ops: 3,
        searcher: None,
        expected_verdict_rate: None,
    },
    CorpusCase {
        id: "av-det-liquidation",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "liquidation",
        min_ops: 1,
        searcher: None,
        expected_verdict_rate: None,
    },
];

/// Detector `Strategy` → realized `MevKind` taxonomy (mirrors
/// `explorer::validate::strategy_to_kind`).
fn strategy_kind(strategy: Strategy) -> Option<&'static str> {
    match strategy {
        Strategy::TwoHopArb | Strategy::MultiHopArb => Some("arb_atomic"),
        Strategy::Sandwich => Some("sandwich"),
        Strategy::Liquidation => Some("liquidation"),
        Strategy::Jit => Some("jit"),
        Strategy::JitArb => Some("jit_arb"),
    }
}

fn repo_cache(name: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("cache")
        .join(name);
    p.exists().then_some(p)
}

/// Seed the temp block cache's `pool_info` registry from the pools the
/// realizer's seed actually swapped on, so `job_run`'s
/// `BacktestRunner::init_pools` has addresses to hydrate over RPC (repo seed
/// block caches hold no registry of their own). DEX + pair only; fee, tick
/// spacing and reserves are re-queried by `init_from_rpc`.
fn seed_pool_registry(blocks: &SqliteStore, explorer: &ExplorerStore) {
    let Ok(rows) = explorer.pools_from_swaps() else {
        eprintln!("WARN: no swap-derived pools to seed registry from");
        return;
    };
    let mut count = 0usize;
    for (pool, amm, token_in, token_out) in rows {
        let Ok(address) = pool.parse::<Address>() else {
            continue;
        };
        let Ok(tin) = token_in.parse::<Address>() else {
            continue;
        };
        let Ok(tout) = token_out.parse::<Address>() else {
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

/// Temp workspace per chain: block-cache copy (pool registry seed when
/// present) + explorer copy (realized reference data). Repo caches are never
/// written.
struct CorpusWorkspace {
    blocks: PathBuf,
    explorer: ExplorerStore,
    explorer_db: PathBuf,
    _dir: PathBuf,
}

impl CorpusWorkspace {
    fn new(chain: ChainName) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("mev_corpus_{chain}_{}", std::process::id()));
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

        // The backtest detector runs off the block-cache pool registry, which
        // repo seeds don't carry; re-seed it from the realizer's swap set.
        if let Ok(blocks_store) = SqliteStore::open(&blocks) {
            seed_pool_registry(&blocks_store, &explorer);
        }

        Ok(CorpusWorkspace {
            blocks,
            explorer,
            explorer_db,
            _dir: dir,
        })
    }
}

/// Runtime config for a corpus chain: defaults + chain + caller RPC, with
/// block fetch / explorer persistence both redirected into the temp workspace
/// (the derived `./cache/…` fallbacks would otherwise mutate repo caches).
#[allow(clippy::field_reassign_with_default)]
fn corpus_config(chain: ChainName, rpc_url: &str, ws: &CorpusWorkspace) -> Config {
    let mut config = Config::default();
    config.chain = chain;
    config.rpc.rpc_url = Some(rpc_url.to_string());
    config.rpc.rps_limit = 5.0; // gentle on free-tier providers (fetch + pool hydration retry)
    config.output.db_path = ws.blocks.to_string_lossy().into_owned();
    config.explorer.db_path = ws.explorer_db.to_string_lossy().into_owned();
    config
}

/// Realized native delta (wei) for a tx: trace evidence first, then the
/// classifier's USD net converted through the seed store's native price.
fn realized_native_wei(store: &ExplorerStore, tx_hash: &str, ts: u64) -> Option<i128> {
    let mut realized: Option<i128> = None;
    for op in store.ops_for_tx(tx_hash).ok()? {
        if let Some(w) = trace_delta_wei(&op) {
            return Some(w);
        }
        if realized.is_none() {
            realized = usd_net_to_wei(store, &op, ts);
        }
    }
    realized
}

fn trace_delta_wei(op: &MevOpRow) -> Option<i128> {
    let v: serde_json::Value = op
        .details_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
    v.get("trace_native_delta_wei")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<i128>().ok())
}

fn usd_net_to_wei(store: &ExplorerStore, op: &MevOpRow, ts: u64) -> Option<i128> {
    let net_usd = op.net_profit_usd?;
    let price = store.native_price_near(ts).ok()??;
    if price <= 0.0 {
        return None;
    }
    Some((net_usd / price * 1e18) as i128)
}

/// Detector expected net profit in native wei.
fn expected_net_wei(opp: &MevOpportunity) -> i128 {
    (opp.expected_profit.to::<u128>() as i128) - (opp.gas_cost_wei as i128)
}

/// Per-detector-opp verdict against the realized reference store.
fn opp_verdict(store: &ExplorerStore, config: &Config, opp: &MevOpportunity) -> MevVerdict {
    let Some(tx) = opp.tx_hash else {
        return MevVerdict::Unverifiable("detector op has no tx anchor".into());
    };
    let expected = expected_net_wei(opp);
    let realized = realized_native_wei(store, &format!("{tx:#x}"), opp.timestamp);
    let abs_wei = match store.native_price_near(opp.timestamp) {
        Ok(Some(p)) if p > 0.0 => (config.explorer.mev_error_usd_tol / p * 1e18) as i128,
        _ => (config.explorer.mev_error_usd_tol * 1e18) as i128,
    };
    let err_pct = match realized {
        Some(r) if expected != 0 => Some((expected - r) as f64 / expected.abs() as f64 * 100.0),
        _ => None,
    };
    mev_verdict(
        Some(expected),
        realized,
        err_pct,
        config.explorer.mev_tolerance_pct,
        abs_wei,
    )
}

/// Detector opps in a case's window, filtered by kind (+ searcher).
fn case_opps<'a>(case: &CorpusCase, all: &'a [MevOpportunity]) -> Vec<&'a MevOpportunity> {
    all.iter()
        .filter(|o| {
            o.block_number >= case.from_block
                && o.block_number <= case.to_block
                && strategy_kind(o.strategy) == Some(case.kind)
                && match case.searcher {
                    Some(s) => o.sender == Some(s),
                    None => true,
                }
        })
        .collect()
}

fn assert_case(store: &ExplorerStore, config: &Config, case: &CorpusCase, all: &[MevOpportunity]) {
    let opps = case_opps(case, all);
    let observed = opps.len() as u64;
    assert!(
        observed >= case.min_ops,
        "{}: expected >= {} {} detector ops in {}..={}, observed {}",
        case.id,
        case.min_ops,
        case.kind,
        case.from_block,
        case.to_block,
        observed
    );

    if let Some(floor) = case.expected_verdict_rate {
        let (pass, fail, unverifiable): (usize, usize, usize) = opps
            .iter()
            .map(|o| opp_verdict(store, config, o))
            .fold((0, 0, 0), |(p, f, u), v| match v {
                MevVerdict::Pass => (p + 1, f, u),
                MevVerdict::Fail(_) => (p, f + 1, u),
                MevVerdict::Unverifiable(_) => (p, f, u + 1),
            });
        let verifiable = pass + fail;
        if verifiable == 0 {
            eprintln!(
                "{}: WARN verdict rate skipped — no verifiable realized data ({} unverifiable; run `MEV_SCOUT_RECORD=1` to seed)",
                case.id, unverifiable
            );
        } else {
            let rate = pass as f64 / verifiable as f64;
            assert!(
                rate >= floor,
                "{}: verdict pass-rate {rate:.2} ({pass}/{verifiable}, {unverifiable} unverifiable) \
                 below floor {floor}",
                case.id
            );
            eprintln!(
                "{}: ok — {observed} {} ops, verdict {pass} pass / {fail} fail / {unverifiable} unverifiable ({rate:.0}% verifiable)",
                case.id, case.kind,
            );
        }
    } else {
        eprintln!("{}: ok — {observed} {} ops", case.id, case.kind);
    }
}

/// `MEV_SCOUT_RECORD=1`: run the union window and print observed facts to
/// seed new `CorpusCase`s (no assertions). Never fails.
async fn record_derived_facts(chain: ChainName, rpc_url: &str) {
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    let union_from = cases.iter().map(|c| c.from_block).min().unwrap_or(0);
    let union_to = cases.iter().map(|c| c.to_block).max().unwrap_or(0);

    let ws = match CorpusWorkspace::new(chain) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Record: workspace failed: {e}");
            return;
        }
    };
    let config = corpus_config(chain, rpc_url, &ws);
    let mut run = config.clone();
    run.from_block = Some(union_from);
    run.to_block = Some(union_to);

    let outcome = match job_run(
        &run,
        &RunOpts {
            // Providers rarely retain `eth_getLogs` deep enough for these
            // historical windows; force a full fetch so detection does not
            // depend on the activity scan.
            fetch_relevant: false,
            ..Default::default()
        },
        &NoopProgress,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Record: detector run failed for {chain}: {e}");
            return;
        }
    };

    let mut by_kind: HashMap<&'static str, (usize, HashMap<String, usize>)> = HashMap::new();
    for opp in &outcome.opportunities {
        let Some(kind) = strategy_kind(opp.strategy) else {
            continue;
        };
        let entry = by_kind.entry(kind).or_insert_with(|| (0, HashMap::new()));
        entry.0 += 1;
        if let Some(sender) = opp.sender {
            *entry.1.entry(format!("{sender:#x}")).or_default() += 1;
        }
    }
    let mut kinds: Vec<_> = by_kind.into_iter().collect();
    kinds.sort_by_key(|b| std::cmp::Reverse(b.1 .0));
    let mut lines = Vec::new();
    lines.push(format!("{chain} window {union_from}..={union_to}"));
    for (kind, (count, searchers)) in kinds {
        lines.push(format!("  kind {kind}: {count} ops"));
        let mut top: Vec<_> = searchers.into_iter().collect();
        top.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (searcher, n) in top.into_iter().take(3) {
            lines.push(format!("    searcher {searcher}: {n}"));
        }
    }
    for case in &cases {
        let opps = case_opps(case, &outcome.opportunities);
        let (pass, fail, unverifiable): (usize, usize, usize) = opps
            .iter()
            .map(|o| opp_verdict(&ws.explorer, &config, o))
            .fold((0, 0, 0), |(p, f, u), v| match v {
                MevVerdict::Pass => (p + 1, f, u),
                MevVerdict::Fail(_) => (p, f + 1, u),
                MevVerdict::Unverifiable(_) => (p, f, u + 1),
            });
        lines.push(format!(
            "  [{}] {} ops — verdict {pass} pass / {fail} fail / {unverifiable} unverifiable",
            case.id,
            opps.len()
        ));
    }
    println!("{}", lines.join("\n"));
}

async fn run_chain_corpus(chain: ChainName, rpc_url: &str) -> bool {
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    if cases.is_empty() {
        return true;
    }

    if std::env::var("MEV_SCOUT_RECORD").is_ok_and(|v| v == "1") {
        record_derived_facts(chain, rpc_url).await;
        return true;
    }

    let ws = match CorpusWorkspace::new(chain) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Skipping: workspace failed for {chain}: {e}");
            return false;
        }
    };
    let config = corpus_config(chain, rpc_url, &ws);

    let union_from = cases.iter().map(|c| c.from_block).min().unwrap();
    let union_to = cases.iter().map(|c| c.to_block).max().unwrap();
    let mut run = config.clone();
    run.from_block = Some(union_from);
    run.to_block = Some(union_to);

    eprintln!(
        "{chain}: detecting over {union_from}..={union_to} (~{} blocks)",
        union_to - union_from + 1
    );
    let outcome = match job_run(
        &run,
        &RunOpts {
            fetch_relevant: false,
            ..Default::default()
        },
        &NoopProgress,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Skipping: detector run failed for {chain}: {e}");
            return false;
        }
    };

    for case in &cases {
        assert_case(&ws.explorer, &config, case, &outcome.opportunities);
    }
    true
}

#[tokio::test]
async fn detector_corpus_matches_derived_facts() {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return;
    };
    let _guard = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let mut chains: Vec<ChainName> = Vec::new();
    for case in CORPUS {
        if !chains.contains(&case.chain) {
            chains.push(case.chain);
        }
    }

    let mut checked = 0usize;
    for chain in chains {
        if run_chain_corpus(chain, &rpc_url).await {
            checked += 1;
        }
    }
    if checked == 0 {
        eprintln!("Skipping: no corpus chain reachable");
    }
}
