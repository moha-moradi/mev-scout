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
