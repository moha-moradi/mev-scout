//! Opt-in network coverage tests.
//!
//! All tests are gated behind `MEV_SCOUT_E2E=1` + RPC reachability and
//! serialized via `rpc_lock()`. Network flakiness is tolerated with
//! SKIP/WARN instead of hard failures, mirroring the other E2E binaries.

#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{ensure_gate_and_rpc, expect_ok, make_cfg, rpc_lock, run_timed, scout, HEAVY_TIMEOUT};

fn tolerant(run: Result<common::TimedOutput, String>, ctx: &str) -> Option<common::TimedOutput> {
    match run {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("SKIP: {ctx} exceeded budget (public-RPC stall):\n{e}");
            None
        }
    }
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
