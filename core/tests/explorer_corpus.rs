//! Real-block golden corpus (WS-H2, first slice).
//!
//! Re-ingests a fixed historical window from a live RPC and asserts derived
//! facts (kind present, op counts above a floor, searcher identity, USD
//! profit floor) that were collected from the seed database produced by
//! `explorer backfill` on Avalanche (blocks 95681722..=95682322).
//!
//! Seed facts (from `cache/explorer-avalanche.sqlite`, Sep 2026):
//! - `arb_atomic` total: 630 ops across the window
//! - `0x93a9f59d5defaae72702dedc2fc4a8fbc287a1ac`: 23 ops, max profit \$2332
//! - `0x92d4ee32bc0f81dbb9923073395a2be7febbc3e5`: 294 ops (dust, max \$0.28)
//! - block 95682057: 9 `arb_atomic` ops, max profit \$682
//! - block 95682033: 1 liquidation by `0xd2a82f1bb41a950ad24829b2f483b1b10f3569dd`
//!
//! Thresholds are deliberately conservative (roughly 60-70% of observed) so
//! small classifier/pricing drift does not fail the corpus, while a missing
//! kind, a lost searcher, or a collapsed USD pipeline still does.
//!
//! Gated like every other live-RPC test: `MEV_SCOUT_E2E=1` + `RPC_URL`
//! (`common::setup::rpc_url`), otherwise it skips with no network I/O.
//!
//! To add a case: pick a block window from the seed DB (or a fresh
//! `explorer backfill`), record kind/searcher/count/profit facts, then append
//! a `CorpusCase` below. Sandwich/jit cases need a window where those kinds
//! occurred — none exist in this seed window.
mod common;
use common::rpc_url;

use alloy::primitives::{address, Address};

use mev_scout_core::config::validation::resolve_chain;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::ingest::{run_range, IngestConfig};
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::explorer::MevKind;
use mev_scout_core::jobs::{init_rpc, load_pool_registry};
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::types::ChainName;

/// One golden real-block expectation.
struct CorpusCase {
    id: &'static str,
    chain: ChainName,
    from_block: u64,
    to_block: u64,
    kind: &'static str,
    min_ops: u64,
    searcher: Option<Address>,
    profit_usd_min: Option<f64>,
}

const CORPUS: &[CorpusCase] = &[
    CorpusCase {
        id: "av-arb-cluster",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 500,
        searcher: None,
        profit_usd_min: None,
    },
    CorpusCase {
        id: "av-arb-whale",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 15,
        searcher: Some(address!("93a9f59d5defaae72702dedc2fc4a8fbc287a1ac")),
        profit_usd_min: Some(1000.0),
    },
    CorpusCase {
        id: "av-arb-dust-searcher",
        chain: ChainName::Avalanche,
        from_block: 95681724,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 200,
        searcher: Some(address!("92d4ee32bc0f81dbb9923073395a2be7febbc3e5")),
        profit_usd_min: None,
    },
    CorpusCase {
        id: "av-arb-dense-block",
        chain: ChainName::Avalanche,
        from_block: 95682057,
        to_block: 95682057,
        kind: "arb_atomic",
        min_ops: 6,
        searcher: None,
        profit_usd_min: Some(100.0),
    },
    CorpusCase {
        id: "av-liquidation-block",
        chain: ChainName::Avalanche,
        from_block: 95682033,
        to_block: 95682033,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("d2a82f1bb41a950ad24829b2f483b1b10f3569dd")),
        profit_usd_min: None,
    },
];

/// Build the runtime config for a corpus chain: defaults + chain + the
/// caller's RPC. If the repo pool-registry cache exists it is wired in so
/// classification matches the seed run; otherwise the registry degrades to
/// transfer-pairing (best-effort, same as production without a cache).
#[allow(clippy::field_reassign_with_default)]
fn corpus_config(chain: ChainName, rpc_url: &str) -> Config {
    let mut config = Config::default();
    config.chain = chain;
    config.rpc.rpc_url = Some(rpc_url.to_string());

    let cache_path = match std::env::var("MEV_SCOUT_POOL_CACHE") {
        Ok(p) if !p.is_empty() => Some(p),
        _ => {
            let fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("cache")
                .join(format!("{chain}-mev-scout.sqlite"));
            fallback
                .exists()
                .then(|| fallback.to_string_lossy().into_owned())
        }
    };
    if let Some(p) = cache_path {
        config.output.db_path = p;
    }
    config
}

/// Assert one corpus case against an already-ingested store.
fn assert_case(store: &ExplorerStore, case: &CorpusCase) {
    let kind = MevKind::parse(case.kind)
        .unwrap_or_else(|| panic!("{}: invalid kind '{}'", case.id, case.kind));
    let mut ops = store
        .ops_in_range(case.from_block, case.to_block, &[kind])
        .unwrap_or_else(|e| panic!("{}: ops_in_range query failed: {e}", case.id));

    if let Some(searcher) = case.searcher {
        let target = format!("{searcher:#x}").to_ascii_lowercase();
        ops.retain(|o| o.eoa.to_ascii_lowercase() == target);
    }

    let observed = ops.len() as u64;
    assert!(
        observed >= case.min_ops,
        "{}: expected >= {} {} ops in {}..={}, observed {}",
        case.id,
        case.min_ops,
        case.kind,
        case.from_block,
        case.to_block,
        observed
    );

    if let Some(floor) = case.profit_usd_min {
        let best = ops
            .iter()
            .filter_map(|o| o.profit_usd)
            .fold(0.0f64, f64::max);
        assert!(
            best >= floor,
            "{}: expected best {} profit >= ${floor}, observed ${best}",
            case.id,
            case.kind
        );
    }

    eprintln!(
        "{}: ok — {} {} ops in {}..={} (floor {})",
        case.id, observed, case.kind, case.from_block, case.to_block, case.min_ops
    );
}

/// Ingest the union window of all corpus cases for one chain, then assert
/// every case of that chain. Skips (never fails) on RPC/network problems so
/// a flaky endpoint does not turn the corpus red — assertion failures are
/// the only hard failures.
async fn run_chain_corpus(chain: ChainName, rpc_url: &str) -> bool {
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    if cases.is_empty() {
        return true;
    }
    let union_from = cases.iter().map(|c| c.from_block).min().unwrap();
    let union_to = cases.iter().map(|c| c.to_block).max().unwrap();

    let config = corpus_config(chain, rpc_url);
    let setup = match init_rpc(&config, chain, true).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: RPC init failed for {chain}: {e}");
            return false;
        }
    };
    let tip = match setup.rpc.get_block_number().await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Skipping: get_block_number failed for {chain}: {e}");
            return false;
        }
    };
    if tip < union_to {
        eprintln!(
            "Skipping: RPC tip {tip} is below corpus window {union_to} — \
             wrong chain, or the endpoint is far behind"
        );
        return false;
    }

    let (_, chain_cfg) = match resolve_chain(&config) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Skipping: resolve_chain failed for {chain}: {e}");
            return false;
        }
    };
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;

    let pool_tokens = load_pool_registry(&config, &chain);
    let store = match ExplorerStore::open_in_memory() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: in-memory store open failed: {e}");
            return false;
        }
    };

    eprintln!(
        "{chain}: ingesting corpus window {union_from}..={union_to} (~{} blocks)",
        union_to - union_from + 1
    );
    if let Err(e) = run_range(
        &setup.rpc,
        &store,
        &cfg,
        &pool_tokens,
        union_from,
        union_to,
        &NoopProgress,
    )
    .await
    {
        eprintln!("Skipping: corpus ingest failed for {chain}: {e}");
        return false;
    }

    for case in cases {
        assert_case(&store, case);
    }
    true
}

#[tokio::test]
async fn real_block_corpus_matches_derived_facts() {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return;
    };

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
