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

#[test]
fn discover_hybrid_incremental_and_flags() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    // Batch-size warning is printed for values above the 5000 recommendation.
    let warn_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &warn_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "1",
        "--batch-size",
        "6000",
        "--json",
    ]);
    if let Some(out) = tolerant(
        run_timed(&mut c, HEAVY_TIMEOUT),
        "discover batch-size warning",
    ) {
        expect_ok(&out, "discover with --batch-size 6000");
        assert!(
            out.combined().contains("exceeds recommended maximum"),
            "expected the >5000 batch-size warning, got:\n{}",
            out.combined()
        );
    }

    let base_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &base_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "2",
        "--solidly-fee-bps",
        "30",
        "--json",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover baseline") {
        expect_ok(&out, "discover onchain with --solidly-fee-bps 30");

        let mut c = scout(&ws);
        c.args([
            "-f",
            &base_cfg,
            "discover",
            "--incremental",
            "--blocks",
            "2",
            "--json",
        ]);
        if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover incremental") {
            expect_ok(&out, "discover --incremental after baseline");
        }
    }

    // hybrid — union of onchain + remote; tolerant to remote-side failures.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &base_cfg,
        "discover",
        "--source",
        "hybrid",
        "--max-pools",
        "20",
        "--json",
    ]);
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
}

#[test]
fn discover_remote_option_flags_enrich_min_tvl_resolve_metadata() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc_opts") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();
    let cfg = make_cfg(&ws, &[("db_path", &db_s)]);

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--max-pools",
        "20",
        "--json",
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

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--min-tvl",
        "100000",
        "--max-pools",
        "20",
        "--json",
    ]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover --min-tvl") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("min-tvl discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                for p in entries {
                    if let Some(tvl) = p.get("tvl_usd").and_then(|v| v.as_f64()) {
                        assert!(
                            tvl >= 100_000.0,
                            "--min-tvl 100000 must filter pools below the floor, got {tvl}"
                        );
                    }
                }
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --min-tvl discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--resolve-remote-metadata",
        "--max-pools",
        "10",
        "--json",
    ]);
    match tolerant(
        run_timed(&mut c, EXTRA_HEAVY),
        "discover --resolve-remote-metadata",
    ) {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("resolve-remote-metadata discover success must print a JSON array");
            assert!(pools.as_array().is_some(), "output must be an array");
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --resolve-remote-metadata discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }
}

#[test]
fn batch_rpc_smoke_run() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_batch") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    let run_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &run_cfg, "run", "--batch-rpc", "--blocks", "2"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "run --batch-rpc") {
        expect_ok(&out, "run --batch-rpc 2 blocks");
        let report_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
        let mut c2 = scout(&ws);
        c2.args(["-f", &report_cfg, "report"]);
        if let Some(out2) = tolerant(
            run_timed(&mut c2, common::TEST_TIMEOUT),
            "report after batch-rpc run",
        ) {
            expect_ok(&out2, "report after batch-rpc run");
            assert!(
                out2.stdout.contains("Run ID:"),
                "report should contain Run ID"
            );
        }
    }
}
