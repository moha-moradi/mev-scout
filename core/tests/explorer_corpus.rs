//! Real-block golden corpus (WS-H2, first slice).
//!
//! Re-ingests fixed historical windows from a live RPC and asserts derived
//! facts (kind present, op counts above a floor, searcher identity, USD
//! profit floor). Expected numbers were collected with `explorer backfill`
//! and are not exact — thresholds sit near 60-70% of what that run stored.
//!
//! Avalanche seed (`cache/explorer-avalanche.sqlite`, blocks 95681722..=95682322):
//! - `arb_atomic` total: 630 ops
//! - `0x93a9f59d5defaae72702dedc2fc4a8fbc287a1ac`: 23 ops, max profit \$2332
//! - `0x92d4ee32bc0f81dbb9923073395a2be7febbc3e5`: 294 ops (dust, max \$0.28)
//! - block 95682057: 9 `arb_atomic` ops, max profit \$682
//! - block 95682033: 1 liquidation by `0xd2a82f1bb41a950ad24829b2f483b1b10f3569dd`
//!
//! Polygon seed (backfill 94430000..=94430280 and block 94430973, 2026-09-25):
//! - `arb_atomic`: 506 ops in the 281-block window
//! - `0x8695330488513a6c3698a2b072ff88aaedbfac3e`: 9 ops, max profit \$62
//! - `0x8c771167beba08d15acd9bee460bc3d14a35201c`: 30 ops
//! - `0xa6563baa758a7c39960a339750c89663aadf9fd2`: 17 ops
//! - block 94430259: 5 `arb_atomic` ops, max profit \$25
//! - blocks 94430973..=94431381: 8 liquidations by
//!   `0x7f1aacb852a7457e8eb97e5196276b92f408d990`
//!
//! Ethereum seed (backfill 26055651..=26055745, 2026-09-25): 5 `sandwich`
//! ops. Three are from `0xae2fc483527b8ef99eb5d9b44875f005ba1fae13`, best
//! priced profit about \$10. No `jit` mint/burn pair landed in that window.
//!
//! Distant windows on one chain are ingested separately (see `ingest_windows`).
//!
//! Gated like every other live-RPC test: `MEV_SCOUT_E2E=1` + `RPC_URL`
//! (`common::setup::rpc_url`), otherwise it skips with no network I/O.
//!
//! ## Seeding new cases (e.g. the missing `jit` kind)
//!
//! `MEV_SCOUT_RECORD=1` swaps the assertions for a report of the observed
//! per-kind / per-searcher / USD facts, printing every kind — a zero line is
//! how a rare kind is confirmed absent rather than merely unlisted. Without a
//! hunt window it re-reports the existing corpus windows.
//!
//! ```text
//! # re-record the current corpus windows
//! set MEV_SCOUT_E2E=1 & set RPC_URL=… & set MEV_SCOUT_RECORD=1
//! cargo test -p mev-scout-core --test explorer_corpus -- --nocapture
//!
//! # hunt a jit window outside the seeded ones. _CHAIN makes the run a hunt for
//! # that chain only (the other chains' corpus windows are not re-ingested);
//! # _FROM and _TO are both required and apply to that chain alone.
//! set MEV_SCOUT_RECORD_CHAIN=ethereum & set MEV_SCOUT_RECORD_FROM=… & set MEV_SCOUT_RECORD_TO=…
//! cargo test -p mev-scout-core --test explorer_corpus -- --nocapture
//! ```
//!
//! Then transcribe the report into a `CorpusCase`: take the `kind jit` count as
//! the `min_ops` floor and ~60-70% of `best profit` as `profit_usd_min` (the
//! same margin the seeds above use), and copy the top `searcher` verbatim.
//! `profit_usd` is only populated when a pool/price cache is available
//! (`MEV_SCOUT_POOL_CACHE`, else `cache/{chain}-mev-scout.sqlite`); without one
//! every USD field reads `$0.00` and `profit_usd_min` should stay `None`.
mod common;
use common::rpc_url;

use std::collections::HashMap;

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
    CorpusCase {
        id: "poly-arb-window",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 350,
        searcher: None,
        profit_usd_min: None,
    },
    CorpusCase {
        id: "poly-arb-whale",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 6,
        searcher: Some(address!("8695330488513a6c3698a2b072ff88aaedbfac3e")),
        profit_usd_min: Some(20.0),
    },
    CorpusCase {
        id: "poly-arb-searcher",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 20,
        searcher: Some(address!("8c771167beba08d15acd9bee460bc3d14a35201c")),
        profit_usd_min: None,
    },
    CorpusCase {
        id: "poly-arb-searcher-2",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 10,
        searcher: Some(address!("a6563baa758a7c39960a339750c89663aadf9fd2")),
        profit_usd_min: None,
    },
    CorpusCase {
        id: "poly-arb-dense-block",
        chain: ChainName::Polygon,
        from_block: 94_430_259,
        to_block: 94_430_259,
        kind: "arb_atomic",
        min_ops: 3,
        searcher: None,
        profit_usd_min: Some(10.0),
    },
    CorpusCase {
        id: "poly-liquidation-window",
        chain: ChainName::Polygon,
        from_block: 94_430_973,
        to_block: 94_431_381,
        kind: "liquidation",
        min_ops: 5,
        searcher: Some(address!("7f1aacb852a7457e8eb97e5196276b92f408d990")),
        profit_usd_min: None,
    },
    CorpusCase {
        id: "eth-sandwich-window",
        chain: ChainName::Ethereum,
        from_block: 26_055_651,
        to_block: 26_055_745,
        kind: "sandwich",
        min_ops: 3,
        searcher: None,
        profit_usd_min: Some(5.0),
    },
    CorpusCase {
        id: "eth-sandwich-searcher",
        chain: ChainName::Ethereum,
        from_block: 26_055_651,
        to_block: 26_055_745,
        kind: "sandwich",
        min_ops: 2,
        searcher: Some(address!("ae2fc483527b8ef99eb5d9b44875f005ba1fae13")),
        profit_usd_min: Some(5.0),
    },
    // Real JIT round trip on UniswapV3 pool 0xd31d41df… (block 26059586):
    // owner 0x1f2f10d1… mints liquidity over ticks [-129060, -129000] and burns
    // the same liquidity later in the block, with an in-range swap at tick
    // -129014 in between, so the position was live for the swap and earned the
    // fee. Recorded with MEV_SCOUT_RECORD=1 over this single block.
    // `profit_usd_min` is unset because a jit op carries no realized profit:
    // it is a fee capture, priced from the pool's in-range swap volume.
    CorpusCase {
        id: "eth-jit-v3-round-trip",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_586,
        kind: "jit",
        min_ops: 1,
        searcher: Some(address!("1f2f10d1c40777ae1da742455c65828ff36df387")),
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

/// Merge case ranges that sit within `GAP` blocks of each other. Distant
/// windows on the same chain are ingested on their own so the harness does
/// not replay every block between them.
fn ingest_windows(cases: &[&CorpusCase]) -> Vec<(u64, u64)> {
    const GAP: u64 = 64;
    let mut ranges: Vec<(u64, u64)> = cases.iter().map(|c| (c.from_block, c.to_block)).collect();
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (from, to) in ranges {
        if let Some(last) = out.last_mut() {
            if from <= last.1.saturating_add(GAP) {
                last.1 = last.1.max(to);
                continue;
            }
        }
        out.push((from, to));
    }
    out
}

/// Ingest each cluster of corpus cases for one chain, then assert every case.
/// Skips (never fails) on RPC/network problems so a flaky endpoint does not
/// turn the corpus red — assertion failures are the only hard failures.
async fn run_chain_corpus(chain: ChainName, rpc_url: &str) -> bool {
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    // A hunt window applies only to the chain it names, so an Ethereum range is
    // never replayed against the Avalanche/Polygon corpus windows.
    let windows = record_windows(chain).unwrap_or_else(|| ingest_windows(&cases));
    if windows.is_empty() {
        return true;
    }
    let union_to = windows.iter().map(|(_, to)| *to).max().unwrap();

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

    for (from, to) in &windows {
        eprintln!(
            "{chain}: ingesting corpus window {from}..={to} (~{} blocks)",
            to - from + 1
        );
        if let Err(e) = run_range(
            &setup.rpc,
            &store,
            &cfg,
            &pool_tokens,
            *from,
            *to,
            &NoopProgress,
        )
        .await
        {
            eprintln!("Skipping: corpus ingest failed for {chain} {from}..={to}: {e}");
            return false;
        }
    }

    if recording() {
        record_report(&store, &windows);
    } else {
        for case in cases {
            assert_case(&store, case);
        }
    }
    true
}

/// Every kind the classifier can emit. A record run reports all of them —
/// including the ones with a zero count, which is how an absent rare kind
/// (`jit`) is confirmed absent rather than merely unlisted.
const ALL_KINDS: &[&str] = &[
    "arb_atomic",
    "sandwich",
    "frontrun",
    "backrun",
    "liquidation",
    "jit",
    "jit_arb",
    "unknown",
];

/// `MEV_SCOUT_RECORD=1` — report derived facts instead of asserting them.
fn recording() -> bool {
    std::env::var("MEV_SCOUT_RECORD").is_ok_and(|v| v == "1")
}

/// The hunt window (`MEV_SCOUT_RECORD_FROM` + `_TO`) for `chain`. Both bounds
/// are required, and the window applies *only* to a chain named by
/// `MEV_SCOUT_RECORD_CHAIN` — otherwise the corpus windows of that chain are
/// used, so a range is never replayed against the wrong chain's blocks.
fn record_windows(chain: ChainName) -> Option<Vec<(u64, u64)>> {
    if !record_chains().contains(&chain) {
        return None;
    }
    let from = std::env::var("MEV_SCOUT_RECORD_FROM").ok()?;
    let to = std::env::var("MEV_SCOUT_RECORD_TO").ok()?;
    parse_hunt_window(&from, &to)
}

/// Parse the hunt window; `None` on a non-numeric or inverted range, which
/// leaves the corpus windows in place.
fn parse_hunt_window(from: &str, to: &str) -> Option<Vec<(u64, u64)>> {
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = to.trim().parse().ok()?;
    (from <= to).then(|| vec![(from, to)])
}

/// Extra chains named by `MEV_SCOUT_RECORD_CHAIN` (comma-separated), so a hunt
/// can run on a chain the corpus does not cover yet.
fn record_chains() -> Vec<ChainName> {
    let Ok(raw) = std::env::var("MEV_SCOUT_RECORD_CHAIN") else {
        return Vec::new();
    };
    parse_chain_list(&raw)
}

/// Unknown names are skipped rather than aborting the hunt.
fn parse_chain_list(raw: &str) -> Vec<ChainName> {
    raw.split(',')
        .filter_map(|c| c.trim().parse::<ChainName>().ok())
        .collect()
}

/// Print observed per-kind / per-searcher / USD facts for the ingested
/// windows. Everything a `CorpusCase` asserts is visible here, so a recorded
/// run can be transcribed into a case field-for-field.
fn record_report(store: &ExplorerStore, windows: &[(u64, u64)]) {
    let mut lines = Vec::new();
    for (from, to) in windows {
        lines.push(format!("{from}..={to}"));
        let ops = match store.ops_in_range(*from, *to, &[]) {
            Ok(ops) => ops,
            Err(e) => {
                lines.push(format!("  ops query failed: {e}"));
                continue;
            }
        };
        for kind in ALL_KINDS {
            let of_kind: Vec<_> = ops.iter().filter(|o| o.kind == *kind).collect();
            if of_kind.is_empty() {
                lines.push(format!("  kind {kind}: 0 ops"));
                continue;
            }
            let mut searchers: HashMap<&str, usize> = HashMap::new();
            let mut best_profit = 0.0f64;
            let mut best_net = 0.0f64;
            for op in &of_kind {
                *searchers.entry(op.eoa.as_str()).or_default() += 1;
                best_profit = best_profit.max(op.profit_usd.unwrap_or(0.0));
                best_net = best_net.max(op.net_profit_usd.unwrap_or(0.0));
            }
            lines.push(format!(
                "  kind {kind}: {} ops, best profit ${best_profit:.2}, best net ${best_net:.2}",
                of_kind.len()
            ));
            let mut top: Vec<_> = searchers.into_iter().collect();
            top.sort_by_key(|b| std::cmp::Reverse(b.1));
            for (searcher, n) in top.into_iter().take(3) {
                lines.push(format!("    searcher {searcher}: {n}"));
            }
            if let Some(best) = of_kind.iter().max_by(|a, b| {
                a.profit_usd
                    .unwrap_or(0.0)
                    .total_cmp(&b.profit_usd.unwrap_or(0.0))
            }) {
                lines.push(format!(
                    "    best tx {} in block {} (eoa {})",
                    best.tx_hash, best.block_number, best.eoa
                ));
            }
        }
    }
    println!("{}", lines.join("\n"));
}

#[tokio::test]
async fn real_block_corpus_matches_derived_facts() {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return;
    };

    // A hunt (`MEV_SCOUT_RECORD_CHAIN`) is about the named chain only, so it
    // does not also re-ingest every other chain's corpus windows. Without
    // `_CHAIN` the run re-records the whole corpus.
    let hunt = record_chains();
    let mut chains: Vec<ChainName> = Vec::new();
    if hunt.is_empty() {
        for case in CORPUS {
            if !chains.contains(&case.chain) {
                chains.push(case.chain);
            }
        }
    } else {
        chains = hunt.clone();
    }

    let mut checked = 0usize;
    for chain in &chains {
        if run_chain_corpus(*chain, &rpc_url).await {
            checked += 1;
        }
    }
    if checked == 0 {
        eprintln!("Skipping: no corpus chain reachable");
    }
    // A named chain with no cases and no usable hunt window has nothing to
    // ingest, so the record run would exit silently — say so.
    if recording() {
        for chain in &hunt {
            if record_windows(*chain).is_none() && !CORPUS.iter().any(|case| case.chain == *chain) {
                eprintln!(
                    "Record: {chain} has no corpus cases and no MEV_SCOUT_RECORD_FROM/_TO \
                     window — nothing to report"
                );
            }
        }
    }
}

#[test]
fn hunt_window_parses_and_rejects_bad_input() {
    assert_eq!(parse_hunt_window(" 100 ", "200"), Some(vec![(100, 200)]));
    assert_eq!(parse_hunt_window("200", "200"), Some(vec![(200, 200)]));
    assert_eq!(parse_hunt_window("200", "100"), None, "inverted range");
    assert_eq!(parse_hunt_window("abc", "200"), None, "non-numeric");
    assert_eq!(parse_hunt_window("", "200"), None, "missing bound");
}

#[test]
fn hunt_chain_list_skips_unknown_names() {
    assert_eq!(
        parse_chain_list("ethereum, polygon"),
        vec![ChainName::Ethereum, ChainName::Polygon]
    );
    assert_eq!(
        parse_chain_list(" ethereum , , base "),
        vec![ChainName::Ethereum, ChainName::Base]
    );
    assert!(parse_chain_list("notachain").is_empty());
    assert!(parse_chain_list("").is_empty());
}

#[test]
fn every_classifier_kind_is_reported_by_a_record_run() {
    // A kind absent from ALL_KINDS would print no line at all, so a rare kind
    // like `jit` could never be confirmed absent.
    for kind in [
        "arb_atomic",
        "sandwich",
        "frontrun",
        "backrun",
        "liquidation",
        "jit",
        "jit_arb",
        "unknown",
    ] {
        assert!(
            ALL_KINDS.contains(&kind),
            "{kind} is emittable but missing from ALL_KINDS"
        );
    }
}
