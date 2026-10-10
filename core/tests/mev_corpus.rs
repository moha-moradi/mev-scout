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
//! facts without asserting — record those into a `CorpusCase` below.
//!
//! `jit` is the remaining gap on the *detector* side. (`sandwich` no longer
//! applies: the live detector was pruned — the explorer still classifies
//! `MevKind::Sandwich`, which is a separate taxonomy.) The realizer corpus
//! already carries a window for it (`eth-jit-v3-round-trip`, Ethereum 26059586
//! — see `explorer_corpus.rs`), so a detector sweep over those same blocks is
//! what turns it into a case here.
//! To hunt a window outside the seeded ones, set
//! `MEV_SCOUT_RECORD_CHAIN=ethereum` + `MEV_SCOUT_RECORD_FROM` +
//! `MEV_SCOUT_RECORD_TO` (both bounds required, that chain only, exactly as
//! `explorer_corpus.rs` does). A one-block window must be a `from == to` hunt
//! ([`apply_window`] routes it through the `block` field).
//!
//! ### Why `jit` is still uncovered: replay fidelity, not seeding
//!
//! A `MEV_SCOUT_RECORD=1` sweep of the explorer's jit block (Ethereum
//! 26059586, `eth-jit-v3-round-trip`) recorded 8 `arb_atomic` ops and **0
//! `jit`**, even though the realizer classifies a mint → swap → burn round trip
//! there. The pool registry is *not* the problem: the jit pool
//! (`0xd31d41df…`, `v3`) is in the explorer's `swaps` table, so
//! [`seed_pool_registry`] seeds and hydrates it (25 puts → 22 distinct pools,
//! which matches the run's "Loaded 22 pools from discovery cache").
//!
//! The replayer reports:
//!
//! ```text
//! Block 26059586 tx 0x0c80d7d2…: status (exec=false, receipt=true),
//!   gas_used (exec=717946, receipt=845345), log_count (exec=0, receipt=36)
//! ```
//!
//! **The local execution reverts partway through the router call**, emitting
//! **0 of the 36** logs — no `Mint`, no `Swap` — while the same tx succeeded
//! on-chain. `JitDetector` matches a mint against a swap log, so with no logs it
//! cannot fire, and the burn tx (`0x11ee3ade…`, the realizer's
//! `burn_tx_index=2`) fails the same way. The realizer still classifies the round
//! trip because it reads the receipt, which carries all 36 logs.
//!
//! Three details of that warning matter when reading it, because none of them is
//! what the wording suggests:
//!
//! - **It is a `Revert`, not a `Halt`.** The warning can only be emitted from
//!   `build_executed_tx` (`replay/replayer.rs`), which is reached solely from
//!   the `Ok(result)` arm of `exec_or_revert` (`:349-376`); the other arms go to
//!   `synthesize_tx`, which never cross-checks a receipt. So the EVM returned a
//!   real `ExecutionResult`, and `exec=717946` is below both the receipt's
//!   `845345` and — necessarily — the tx's own gas limit, which must be at least
//!   `845345` for the on-chain tx to have succeeded. A `Halt` burns the entire
//!   gas limit, so it is excluded, as is the synthetic-revert arm (which also
//!   reports the full limit). A `Revert` returns unspent gas, which matches.
//! - **The logs were never emitted, not discarded afterwards.** revm's
//!   `ExecutionResult::logs()` yields the log list for `Success`, `Revert` and
//!   `Halt` alike, so `log_count (exec=0)` means execution produced none before
//!   stopping.
//! - **The database answered every read.** A state-layer failure arrives as
//!   `EVMError::Database` → `StateLoadFailed` → `synthesize_tx`, which cannot
//!   produce this warning. So the revert comes from *divergent state*, not a
//!   missing account: a pruned provider answering `eth_getStorageAt` with `0x0`
//!   (`replay/db.rs` `storage_ref` → `RpcClient::get_storage_at`) is enough to
//!   trip a router's own guard. That matches the RPC requirement noted below.
//!
//! The asymmetry generalizes: `jit` is **log-based** — it needs logs for the
//! transaction, nothing else — while `arb_atomic` reads
//! `PoolManager` reserves and so survives a failed replay (that is why the same
//! block still yielded 8 arb ops; its reserves are merely stale, not empty). A
//! zero-op window for a log-based kind is therefore not evidence the kind is
//! absent: check the replayer's `status (exec=false, receipt=true)` warnings
//! first.
//!
//! ### The lever, and why it is not pulled
//!
//! The receipt's logs are already in hand and are simply dropped:
//! `build_executed_tx` reads `gas_used` from the receipt when present
//! (`replay/replayer.rs`) but takes `logs` from `exec_result` only. A fallback —
//! use the receipt's logs when execution produced none *and* the receipt reports
//! success — would hand all 36 logs to the detectors, and the pool manager
//! advances from the same `tx.logs` (`pipeline/runner.rs`), so state maintenance
//! would follow too. Nothing else is missing: the three detectors take
//! `&[ExecutedLog]` plus `&PoolManager` metadata and fire on logs alone, the
//! `LogData → ExecutedLog` conversion already exists in `synthesize_tx`, and
//! there is no post-state channel to fix up (`run_block` binds the committed DB
//! as `_db` and drops it).
//!
//! It is not applied because it changes the reserve trajectory of every block
//! containing a divergent transaction, which would invalidate the seeded
//! `eth-det-arb-block-26059586` floor until that block is re-recorded — and a
//! record run needs an archive-state RPC, which this environment does not have.
//! Unvalidated, it would also be a silent change to production detection. Seeding
//! a `jit` case is therefore two steps, in this order: decide on the fallback,
//! then re-record. Until then a hunt should target windows whose transactions
//! replay successfully, which argues for the Avalanche/Polygon windows the plan
//! names (cheap blocks, and the existing Avalanche detector cases replay
//! cleanly) over dense mainnet blocks.
//!
//! Unlike the realizer sweep, a detector record run needs seed data on both
//! sides: `job_run` hydrates pools from the chain's block cache, which is
//! seeded here from the explorer's swap-derived pool registry
//! ([`seed_pool_registry`]). A chain with no `cache/explorer-{chain}.sqlite`
//! therefore has no pool registry and records a zero-op window, not a real
//! "kind absent" reading.
//!
//! ### Running a hunt
//!
//! Replay of a mainnet-dense block is CPU- and RPC-bound: the EVM interpreter
//! needs more stack than the 2 MiB test-thread default (a `STATUS_STACK_OVERFLOW`
//! on the first complex router call), and a single Ethereum block is ~15 min at
//! the harness's 5 rps. Run hunts in release, with a raised stack:
//!
//! ```text
//! set RUST_MIN_STACK=134217728
//! set MEV_SCOUT_E2E=1 & set RPC_URL=… & set MEV_SCOUT_RECORD=1
//! set MEV_SCOUT_RECORD_CHAIN=ethereum & set MEV_SCOUT_RECORD_FROM=… & set MEV_SCOUT_RECORD_TO=…
//! cargo test --release -p mev-scout-core --test mev_corpus -- --nocapture
//! ```
//!
//! The RPC must serve *state* at the window (drpc / mevblocker do; plain
//! publicnode does not), or pool hydration fails and the window records zeros.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;
use common::rpc_url;

use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::{address, Address};

use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::jobs::{job_run, RunOpts};
use mev_scout_core::mev::{mev_verdict, MevVerdict};
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::types::{ChainName, GasModel, MevOpportunity, Strategy};

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
    // Ethereum seed, recorded with `MEV_SCOUT_RECORD=1` on the explorer's own
    // jit block (26059586, `eth-jit-v3-round-trip` in `explorer_corpus.rs`), so
    // the T1 cross-check (`explorer validate`) is exercisable on the same block
    // for both layers. Observed: 8 `arb_atomic` detector ops, 7 of them by
    // `0xae2fc483…` — the same searcher the realizer attributes the Ethereum
    // sandwiches to. No verdict band is asserted: the record run reports
    // per-kind counts and searchers, and a verdict floor needs a case to
    // evaluate, so it is added only once a re-record shows the rate.
    CorpusCase {
        id: "eth-det-arb-block-26059586",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_586,
        kind: "arb_atomic",
        min_ops: 5,
        searcher: Some(address!("ae2fc483527b8ef99eb5d9b44875f005ba1fae13")),
        expected_verdict_rate: None,
    },
];

/// Detector `Strategy` → realized `MevKind` taxonomy (mirrors
/// `explorer::validate::strategy_to_kind`).
fn strategy_kind(strategy: Strategy) -> Option<&'static str> {
    match strategy {
        Strategy::TwoHopArb | Strategy::MultiHopArb => Some("arb_atomic"),
        Strategy::Jit => Some("jit"),
        Strategy::Backrun => Some("backrun"),
    }
}

/// Every kind the detector can emit. A record run prints all of them, so a
/// kind it found nothing for reads as an explicit `0 ops` line — that is how a
/// rare kind (`jit`) is *confirmed absent* over a hunt window rather than
/// merely unlisted. Note this is the *detector* taxonomy: the live `Sandwich`
/// and `JitArb` detectors are gone, and liquidation capture has no `Strategy`
/// variant at all, so `sandwich`/`jit_arb`/`liquidation` are never produced
/// here even though the explorer still classifies all three as a `MevKind`.
const ALL_KINDS: &[&str] = &["arb_atomic", "jit", "backrun"];

/// `MEV_SCOUT_RECORD=1` — report derived facts instead of asserting them.
fn recording() -> bool {
    std::env::var("MEV_SCOUT_RECORD").is_ok_and(|v| v == "1")
}

/// The hunt window (`MEV_SCOUT_RECORD_FROM` + `_TO`) for `chain`. Both bounds
/// are required, and the window applies *only* to a chain named by
/// `MEV_SCOUT_RECORD_CHAIN` — otherwise that chain's corpus windows are used,
/// so a range is never run against the wrong chain's blocks.
fn hunt_window(chain: ChainName) -> Option<(u64, u64)> {
    if !hunt_chains().contains(&chain) {
        return None;
    }
    let from = std::env::var("MEV_SCOUT_RECORD_FROM").ok()?;
    let to = std::env::var("MEV_SCOUT_RECORD_TO").ok()?;
    parse_hunt_window(&from, &to)
}

/// Parse the hunt window; `None` on a non-numeric or inverted range, which
/// leaves the corpus windows in place.
fn parse_hunt_window(from: &str, to: &str) -> Option<(u64, u64)> {
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = to.trim().parse().ok()?;
    (from <= to).then_some((from, to))
}

/// Chains named by `MEV_SCOUT_RECORD_CHAIN` (comma-separated), so a hunt can
/// run on a chain the detector corpus does not cover yet.
fn hunt_chains() -> Vec<ChainName> {
    let Ok(raw) = std::env::var("MEV_SCOUT_RECORD_CHAIN") else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|c| c.trim().parse::<ChainName>().ok())
        .collect()
}

/// Window a record run covers for `chain`: the hunt window when one is given,
/// else the union of that chain's corpus cases.
fn record_range(chain: ChainName) -> Option<(u64, u64)> {
    if let Some(w) = hunt_window(chain) {
        return Some(w);
    }
    let cases = CORPUS.iter().filter(|c| c.chain == chain);
    let from = cases.clone().map(|c| c.from_block).min()?;
    let to = cases.map(|c| c.to_block).max()?;
    Some((from, to))
}

/// Apply a window to a run config. A one-block window goes through the
/// `block` field, not `from_block`/`to_block`: `BlockRange::from_to` rejects
/// `to == from` (`config::validation`), so `from/to` cannot express the
/// single-block kind cases the explorer corpus is seeded with.
fn apply_window(config: &mut Config, from_block: u64, to_block: u64) {
    if from_block == to_block {
        config.block = Some(from_block);
    } else {
        config.from_block = Some(from_block);
        config.to_block = Some(to_block);
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

/// Pin the gas inputs a corpus run must not inherit from a moving default
/// (MEV-VERIFICATION §D.3).
///
/// `Config::default()` happens to land on `HistoricalExact` via `GasModel`'s
/// `#[default]`, but a corpus seed that is only deterministic *by accident* is a
/// seed that goes stale the moment that default moves. `HistoricalExact` is the
/// one model that ignores `percentile_gas_price`, which `run_range` derives
/// from *prior* blocks (`pipeline/runner.rs`) — every other model makes a
/// block's ops depend on where in the range it sits. A zero premium and fee
/// then keep modeled gas a pure function of the block's own base fee.
fn pin_gas(config: &mut Config) {
    config.gas.gas_model = GasModel::HistoricalExact;
    config.gas.priority_fee_gwei = 0.0;
    config.gas.winning_bid_premium = 0.0;
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
    pin_gas(&mut config);
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

/// `MEV_SCOUT_RECORD=1`: run the hunt window when one is given, else the union
/// of the chain's corpus windows, and print observed facts to seed new
/// `CorpusCase`s (no assertions). Never fails.
async fn record_derived_facts(chain: ChainName, rpc_url: &str) {
    let Some((from_block, to_block)) = record_range(chain) else {
        eprintln!(
            "Record: {chain} has no corpus cases and no MEV_SCOUT_RECORD_FROM/_TO window — \
             nothing to report"
        );
        return;
    };
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();

    let ws = match CorpusWorkspace::new(chain) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Record: workspace failed: {e}");
            return;
        }
    };
    let config = corpus_config(chain, rpc_url, &ws);
    let mut run = config.clone();
    apply_window(&mut run, from_block, to_block);

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
            eprintln!("Record: detector run failed for {chain}: {e:#}");
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
    let mut lines = Vec::new();
    lines.push(format!("{chain} window {from_block}..={to_block}"));
    if repo_cache(&format!("explorer-{chain}.sqlite")).is_none() {
        // Without a realizer seed there is no pool registry either (see
        // `seed_pool_registry`), so a zero-op window here is a harness
        // precondition failure, not evidence the kind is absent.
        lines.push(format!(
            "  WARN no cache/explorer-{chain}.sqlite seed — no pool registry, zero-op windows \
             are not a 'kind absent' reading"
        ));
    }
    // Every kind is printed, so an absent one is an explicit 0 line.
    for kind in ALL_KINDS {
        let Some((count, searchers)) = by_kind.remove(*kind) else {
            lines.push(format!("  kind {kind}: 0 ops"));
            continue;
        };
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
    // Record mode comes first: a hunt may target a chain the corpus has no
    // cases for, and that run has work to do.
    if recording() {
        record_derived_facts(chain, rpc_url).await;
        return true;
    }

    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    if cases.is_empty() {
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
    apply_window(&mut run, union_from, union_to);

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

// `job_run` drives `CachedRpcDb`, which blocks on the RPC handle
// (`replay::db`) — that panics on a current-thread runtime, so the flavor is
// pinned here exactly as in `backtest.rs`.
#[tokio::test(flavor = "multi_thread")]
async fn detector_corpus_matches_derived_facts() {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return;
    };
    let _guard = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    // A hunt (`MEV_SCOUT_RECORD_CHAIN`) is about the named chain only, so it
    // does not also sweep every other chain's corpus windows. Without
    // `_CHAIN` the run covers the whole corpus.
    let hunt = hunt_chains();
    let chains: Vec<ChainName> = if hunt.is_empty() {
        let mut seen: Vec<ChainName> = Vec::new();
        for case in CORPUS {
            if !seen.contains(&case.chain) {
                seen.push(case.chain);
            }
        }
        seen
    } else {
        hunt
    };

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

#[test]
fn hunt_window_parses_and_rejects_bad_input() {
    assert_eq!(parse_hunt_window(" 100 ", "200"), Some((100, 200)));
    assert_eq!(parse_hunt_window("200", "200"), Some((200, 200)));
    assert_eq!(parse_hunt_window("200", "100"), None, "inverted range");
    assert_eq!(parse_hunt_window("abc", "200"), None, "non-numeric");
    assert_eq!(parse_hunt_window("", "200"), None, "missing bound");
}

#[test]
fn record_range_falls_back_to_the_corpus_union_window() {
    // No hunt env in the offline test process: each seeded chain resolves to
    // the union of its own cases, never to another chain's blocks.
    assert_eq!(
        record_range(ChainName::Avalanche),
        Some((95681722, 95682322))
    );
    assert_eq!(
        record_range(ChainName::Ethereum),
        Some((26_059_586, 26_059_586))
    );
    // A chain with no cases and no hunt window has nothing to record.
    assert_eq!(record_range(ChainName::Base), None);
}

#[test]
fn a_single_block_window_uses_the_block_field() {
    // `BlockRange::from_to` rejects `to == from`, so a one-block case must go
    // through `block` or the run dies with "invalid configuration".
    let mut cfg = Config::default();
    apply_window(&mut cfg, 26_059_586, 26_059_586);
    assert_eq!(cfg.block, Some(26_059_586));
    assert_eq!(cfg.from_block, None);
    assert_eq!(cfg.to_block, None);

    let mut cfg = Config::default();
    apply_window(&mut cfg, 26_055_651, 26_055_745);
    assert_eq!(cfg.block, None);
    assert_eq!(cfg.from_block, Some(26_055_651));
    assert_eq!(cfg.to_block, Some(26_055_745));
}

#[test]
fn every_detector_kind_is_reported_by_a_record_run() {
    // A kind missing from ALL_KINDS would print no line at all, so a hunt for
    // a rare kind (`jit`) could never confirm it absent.
    for kind in ["arb_atomic", "jit", "backrun"] {
        assert!(ALL_KINDS.contains(&kind), "{kind} missing from ALL_KINDS");
    }
    assert_eq!(ALL_KINDS.len(), 3, "one entry per emittable MevKind");
}

#[test]
fn corpus_gas_inputs_are_pinned_for_determinism() {
    // A `Distribution` model reads `percentile_gas_price`, which `run_range`
    // derives from prior blocks, so a block's ops would depend on its position
    // in the range and the seeds would drift with range length.
    let mut cfg = Config::default();
    pin_gas(&mut cfg);
    assert_eq!(cfg.gas.gas_model, GasModel::HistoricalExact);
    assert_eq!(
        cfg.gas.gas_model.target_percentile(),
        None,
        "a percentile model would make a block's gas depend on the blocks before it"
    );
    assert_eq!(cfg.gas.priority_fee_gwei, 0.0);
    assert_eq!(cfg.gas.winning_bid_premium, 0.0);
}

#[test]
fn a_hollow_corpus_run_would_not_be_read_as_a_missing_kind() {
    // `seed_pool_registry` is the harness precondition: with no swap-derived
    // pools the detector has no pool universe, and a record run reports 0 ops
    // for *every* kind. `record_derived_facts` warns about it, so a hollow run
    // is distinguishable from a genuine "kind absent" reading — this pins the
    // two apart at the unit level.
    let sandbox = std::env::temp_dir().join(format!("mev_corpus_pools_{}", std::process::id()));
    std::fs::create_dir_all(&sandbox).unwrap();
    let blocks = sandbox.join("blocks.sqlite");
    let explorer_db = sandbox.join("explorer.sqlite");

    // An explorer store with no swaps ⇒ no seedable pools.
    let explorer = ExplorerStore::open(&explorer_db).unwrap();
    assert_eq!(
        explorer.pools_from_swaps().unwrap().len(),
        0,
        "a fresh explorer store must yield no swap-derived pools"
    );
    let blocks_store = SqliteStore::open(&blocks).unwrap();
    seed_pool_registry(&blocks_store, &explorer);
    assert_eq!(
        blocks_store.list_discovered_pools().unwrap().len(),
        0,
        "so the registry stays empty and a run over it would be hollow"
    );
    let _ = std::fs::remove_dir_all(&sandbox);
}
