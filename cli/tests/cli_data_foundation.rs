//! Network-facing data-foundation coverage: `discover` over on-chain factory
//! events. All tests are gated behind `MEV_SCOUT_E2E=1` + RPC reachability and
//! serialized via `rpc_lock()`, tolerating provider stalls with SKIP.

mod common;

use common::{
    ensure_gate_and_rpc, expect_fail, expect_ok, extract_json_array, make_cfg, rpc_lock, run_timed,
    scout, TimedOutput, HEAVY_TIMEOUT, TEST_TIMEOUT,
};

fn tolerant(run: Result<TimedOutput, String>, ctx: &str) -> Option<TimedOutput> {
    match run {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("SKIP: {ctx} exceeded budget (public-RPC stall):\n{e}");
            None
        }
    }
}

#[test]
fn data_foundation_pipeline_discover() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf") else {
        return;
    };
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    // Discovery is on-chain factory events only; the source is no longer a
    // user-visible choice.
    let discover_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &discover_cfg, "discover", "--blocks", "5"]);
    let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover baseline") else {
        return;
    };
    expect_ok(&out, "discover onchain 5 blocks");
    let pools = extract_json_array(&out.stdout).unwrap_or_else(|| {
        panic!(
            "discover with output=json did not print a JSON array\nexit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.code, out.stdout, out.stderr
        )
    });
    let entries = pools
        .as_array()
        .expect("discover json output must be an array");
    for p in entries {
        assert!(p.get("address").is_some(), "pool missing address: {p}");
        assert!(p.get("token0").is_some(), "pool missing token0: {p}");
        assert!(p.get("token1").is_some(), "pool missing token1: {p}");
        assert!(p.get("dex_type").is_some(), "pool missing dex_type: {p}");
    }
    assert!(db.exists(), "sqlite db should exist after discover");

    // A second zero-arg run must resume incrementally rather than rescan the
    // whole lookback window.
    let mut c = scout(&ws);
    c.args(["-f", &discover_cfg, "discover"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover implicit") {
        if out.success {
            assert!(
                out.combined().contains("resuming incrementally")
                    || out.combined().contains("Incremental mode"),
                "second discover should take the implicit incremental path, got:\n{}",
                out.combined()
            );
        }
    }
}

/// `discover --source` and `--enrich` are gone: the default path must not
/// depend on a third-party HTTP aggregator. Purely offline — clap rejects the
/// flags before any RPC is touched.
#[test]
fn discover_rejects_removed_source_and_enrich_flags() {
    let ws = common::temp_ws("dataf_flags_removed");
    for args in [
        vec!["discover", "--source", "remote"],
        vec!["discover", "--source", "hybrid"],
        vec!["discover", "--source", "onchain"],
        vec!["discover", "--enrich"],
    ] {
        let mut c = scout(&ws);
        c.args(&args);
        let out = run_timed(&mut c, TEST_TIMEOUT).expect("spawn/wait failed");
        expect_fail(&out, &format!("removed discover flag: {args:?}"));
        assert!(
            out.combined().contains("unexpected argument"),
            "expected a clap rejection for {args:?}, got:\n{}",
            out.combined()
        );
    }
}
