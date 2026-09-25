//! Opt-in network coverage tests.
//!
//! All tests are gated behind `MEV_SCOUT_E2E=1` + RPC reachability and
//! serialized via `rpc_lock()`. Network flakiness is tolerated with
//! SKIP/WARN instead of hard failures, mirroring the other E2E binaries.

mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, extract_json_array, make_cfg, rpc_lock, run_timed, scout,
    HEAVY_TIMEOUT,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

/// 10-minute cap for the whole-file heavy helpers (kept below HEAVY_TIMEOUT).
const EXTRA_HEAVY: Duration = Duration::from_secs(900);

fn tolerant(run: Result<common::TimedOutput, String>, ctx: &str) -> Option<common::TimedOutput> {
    match run {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("SKIP: {ctx} exceeded budget (public-RPC stall):\n{e}");
            None
        }
    }
}

fn append_toml(path: &str, extra: &str) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "\n{extra}").unwrap();
}

#[test]
fn discover_hybrid_incremental_and_enrich() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    let base_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f", &base_cfg, "discover", "--source", "onchain", "--blocks", "2",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover baseline") {
        expect_ok(&out, "discover onchain baseline");

        let mut c = scout(&ws);
        c.args([
            "-f",
            &base_cfg,
            "discover",
            "--incremental",
            "--blocks",
            "2",
        ]);
        if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover incremental") {
            expect_ok(&out, "discover --incremental after baseline");
        }
    }

    // hybrid — union of onchain + remote; tolerant to remote-side failures.
    let hybrid_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    append_toml(&hybrid_cfg, "[discover]\nmax_pools = 20");
    let mut c = scout(&ws);
    c.args(["-f", &hybrid_cfg, "discover", "--source", "hybrid"]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover hybrid") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("hybrid discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                eprintln!("hybrid discovery returned {} pools", entries.len());
                let addrs: Vec<_> = entries
                    .iter()
                    .filter_map(|p| p.get("address").and_then(|a| a.as_str()))
                    .collect();
                let unique: std::collections::HashSet<_> =
                    addrs.iter().map(|s| s.to_lowercase()).collect();
                assert_eq!(
                    addrs.len(),
                    unique.len(),
                    "hybrid union must dedup pools by address"
                );
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): hybrid discover failed (remote aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }

    // --enrich via remote source
    let enrich_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    append_toml(&enrich_cfg, "[discover]\nmax_pools = 20");
    let mut c = scout(&ws);
    c.args([
        "-f",
        &enrich_cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
    ]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover --enrich") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("enriched discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                eprintln!("enriched remote discovery returned {} pools", entries.len());
                for p in entries {
                    assert!(
                        p.get("tvl_usd").is_some() && p.get("volume_usd_24h").is_some(),
                        "enriched pool must carry tvl/volume fields: {p}"
                    );
                }
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --enrich discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }
}

#[test]
fn run_smoke() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_run") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    let run_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &run_cfg, "run", "--blocks", "2"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "run 2 blocks") {
        expect_ok(&out, "run 2 blocks");
        let report_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
        let mut c2 = scout(&ws);
        c2.args(["-f", &report_cfg, "report"]);
        if let Some(out2) = tolerant(run_timed(&mut c2, common::TEST_TIMEOUT), "report after run") {
            expect_ok(&out2, "report after run");
            assert!(
                out2.stdout.contains("Run ID:"),
                "report should contain Run ID"
            );
        }
    }
}

/// `explorer show --trace` against a live op: pushes the trace gate through the
/// full CLI path. Verdict determinism is covered offline by `trace_verdict`
/// unit tests; here we only require the path to either reconcile the op
/// (prints `trace gate:`) or fail gracefully at the trace RPC layer (a
/// provider without `debug_*`), never a panic/hang.
#[test]
fn show_trace_reconciles_live_op() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_show_trace") else {
        return;
    };
    let rpc = common::rpc_url().expect("gate guarantees RPC_URL");
    let db_s = ws.join("explorer.sqlite").to_str().unwrap().to_string();
    let cfg = make_cfg(&ws, &[("rpc_urls", &format!("[\"{rpc}\"]"))]);
    append_toml(&cfg, &format!("[explorer]\ndb_path = \"{db_s}\""));

    // Backfill a small finalized window near the tip so the store has ops.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (from, to) = rt.block_on(async {
        let config = mev_scout_core::config::Config::load_or_default(&cfg).expect("cfg parses");
        let v = mev_scout_core::config::validation::validate_live(&config).expect("chain resolves");
        let chain = v.chain_name;
        let (_, chain_cfg) =
            mev_scout_core::config::validation::resolve_chain(&config).expect("chain config");
        use mev_scout_core::explorer::ingest::{safe_head, IngestConfig};
        use mev_scout_core::jobs::init_rpc;
        let mut icfg = IngestConfig::from_chain(chain, &chain_cfg);
        icfg.confirmations = config.explorer.confirmations;
        icfg.arb_likely_parity = config.explorer.arb_likely_parity;
        let setup = init_rpc(&config, chain, true).await.expect("rpc connects");
        let head = safe_head(&setup.rpc, &icfg).await.expect("safe head");
        (head - 5, head - 1)
    });

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "explorer",
        "backfill",
        "--from-block",
        &from.to_string(),
        "--to-block",
        &to.to_string(),
    ]);
    let Some(out) = tolerant(run_timed(&mut c, EXTRA_HEAVY), "backfill for show --trace") else {
        return;
    };
    expect_ok(&out, "backfill small window");

    let store = mev_scout_core::explorer::store::ExplorerStore::open(&db_s).unwrap();
    let ops = store.ops_in_range(from, to, &[]).unwrap();
    let Some(op) = ops
        .iter()
        .filter(|o| o.profit_usd.is_some())
        .max_by(|a, b| {
            a.profit_usd
                .partial_cmp(&b.profit_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    else {
        eprintln!("SKIP: no priced ops in window {from}..={to} (fine on quiet chains)");
        return;
    };

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "explorer",
        "show",
        &op.tx_hash,
        "--trace",
        "--tolerance-pct",
        "1000000",
    ]);
    let Some(out) = tolerant(run_timed(&mut c, EXTRA_HEAVY), "show --trace") else {
        return;
    };
    assert!(
        out.combined().contains("trace gate:")
            || out.combined().contains("trace failed")
            || out.combined().contains("unable to trace"),
        "show --trace must reconcile the op or degrade gracefully at the RPC layer:\n{}",
        out.combined()
    );
}
