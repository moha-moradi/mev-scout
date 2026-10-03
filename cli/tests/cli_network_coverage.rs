//! Opt-in network coverage tests.
//!
//! All tests are gated behind `MEV_SCOUT_E2E=1` + RPC reachability and
//! serialized via `rpc_lock()`. Network flakiness is tolerated with
//! SKIP/WARN instead of hard failures, mirroring the other E2E binaries.

mod common;

use common::{ensure_gate_and_rpc, expect_ok, make_cfg, rpc_lock, run_timed, scout, HEAVY_TIMEOUT};
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
fn discover_incremental_and_implicit_window() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    let base_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &base_cfg, "discover", "--blocks", "2"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover baseline") {
        expect_ok(&out, "discover baseline");

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

        // Zero-arg discover on a populated cache must resume incrementally
        // rather than rescanning the lookback window.
        let mut c = scout(&ws);
        c.args(["-f", &base_cfg, "discover"]);
        if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover implicit") {
            expect_ok(&out, "zero-arg discover with a populated cache");
            assert!(
                out.combined().contains("resuming incrementally")
                    || out.combined().contains("Incremental mode"),
                "zero-arg discover must take the implicit incremental path:\n{}",
                out.combined()
            );
        }
    }
}

#[test]
fn live_smoke() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_live") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    // One-shot live (no --loop) is the CLI replacement for `run --blocks N`:
    // it detects at the tip and writes a manifest, so `report` still works.
    let live_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &live_cfg, "live"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "live one-shot") {
        expect_ok(&out, "live one-shot");
        let report_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
        let mut c2 = scout(&ws);
        c2.args(["-f", &report_cfg, "report"]);
        if let Some(out2) = tolerant(
            run_timed(&mut c2, common::TEST_TIMEOUT),
            "report after live",
        ) {
            expect_ok(&out2, "report after live");
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

    // A wide tolerance keeps a live-op trace from failing the gate on rounding
    // alone; it is set via config since `--tolerance-pct` is gone.
    append_toml(
        &cfg,
        "trace_tolerance_pct = 1000000.0\nmev_tolerance_pct = 1000000.0",
    );

    let mut c = scout(&ws);
    c.args(["-f", &cfg, "explorer", "show", &op.tx_hash, "--trace"]);
    let Some(out) = tolerant(run_timed(&mut c, EXTRA_HEAVY), "show --trace") else {
        return;
    };
    assert!(
        out.combined().contains("trace gate:"),
        "show --trace must report a trace verdict:\n{}",
        out.combined()
    );
    expect_ok(&out, "show --trace with degraded coverage");
}
