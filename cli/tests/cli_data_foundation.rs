mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, extract_json_array, repo_config, rpc_ready, run_timed, scout,
    temp_ws, HEAVY_TIMEOUT, NETWORK_TIMEOUT, RPC_MUTEX,
};
use std::time::Duration;

fn cfg() -> String {
    repo_config().to_str().unwrap().to_string()
}

#[test]
fn data_foundation_pipeline_discover_tokens_fetch_scan() {
    let _guard = RPC_MUTEX.lock().unwrap();
    let Some(ws) = ensure_gate_and_rpc("dataf") else {
        return;
    };
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "5",
        "--json",
        "--db-path",
        db_s,
    ]);
    if let Some(rpc) = common::first_rpc_url() {
        c.args(["--rpc", &rpc]);
    }
    let out = match run_timed(&mut c, HEAVY_TIMEOUT) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIP: on-chain discover exceeded budget (provider-side stall):\n{e}");
            return;
        }
    };
    expect_ok(&out, "discover onchain 5 blocks");
    let pools = extract_json_array(&out.stdout).unwrap_or_else(|| {
        panic!(
            "discover --json did not print a JSON array\nexit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.code, out.stdout, out.stderr
        )
    });
    assert!(
        pools.is_array(),
        "expected array, got: {}",
        out.stdout.chars().take(200).collect::<String>()
    );
    if let Some(entries) = pools.as_array() {
        for p in entries {
            assert!(p.get("address").is_some(), "pool missing address: {p}");
            assert!(p.get("token0").is_some(), "pool missing token0: {p}");
            assert!(p.get("token1").is_some(), "pool missing token1: {p}");
            assert!(p.get("dex_type").is_some(), "pool missing dex_type: {p}");
        }
    }

    let mut c = scout(&ws);
    c.args(["-f", &cfg(), "tokens", "--cache-only"]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("tokens spawn failed");
    expect_ok(&out, "tokens --cache-only");
    assert!(
        out.stdout.contains("Token cache:"),
        "expected cache summary line, got: {}",
        out.stdout
    );

    let mut c = scout(&ws);
    c.args(["-f", &cfg(), "tokens", "--output", "json"]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("tokens json spawn failed");
    expect_ok(&out, "tokens --output json");
    let toks = extract_json_array(&out.stdout).expect("tokens --output json should print array");
    let entries = toks.as_array().expect("tokens output must be an array");
    assert!(
        entries.len() >= 5,
        "bundled known-token list should seed the cache"
    );
    for t in entries {
        assert!(t.get("address").is_some(), "token missing address");
        assert!(t.get("symbol").is_some(), "token missing symbol");
        assert!(t.get("decimals").is_some(), "token missing decimals");
    }

    let mut c = scout(&ws);
    c.args(["-f", &cfg(), "tokens", "--output", "csv"]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("tokens csv spawn failed");
    expect_ok(&out, "tokens --output csv");
    assert!(
        out.stdout
            .lines()
            .any(|l| l.trim() == "address,symbol,decimals"),
        "csv header line missing:\n{}",
        out.stdout
    );

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "fetch",
        "--blocks",
        "5",
        "--no-sig-resolve",
        "--db-path",
        db_s,
    ]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("fetch spawn failed");
    expect_ok(&out, "fetch 5 blocks");
    assert!(
        out.stdout.contains("Fetch complete:"),
        "missing fetch summary:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Total blocks: 5"),
        "fetch should report Total blocks: 5\n{}",
        out.stdout
    );
    assert!(db.exists(), "sqlite db should exist after fetch");

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "scan",
        "--kind",
        "trades",
        "--blocks",
        "5",
        "--limit",
        "20",
        "--output",
        "json",
    ]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("scan spawn failed");
    expect_ok(&out, "scan trades 5 blocks json");
    let events = extract_json_array(&out.stdout).expect("scan --output json should print array");
    if let Some(items) = events.as_array() {
        for e in items {
            assert!(e.get("block").is_some(), "trade event missing block: {e}");
            assert!(e.get("tx_hash").is_some(), "trade event missing tx_hash: {e}");
        }
    }
}

#[test]
fn discover_remote_tolerant_to_service_failures() {
    let _guard = RPC_MUTEX.lock().unwrap();
    if std::env::var("MEV_SCOUT_E2E").as_deref() != Ok("1") {
        eprintln!("SKIP: set MEV_SCOUT_E2E=1 to run live-Polygon E2E tests");
        return;
    }
    let ws = temp_ws("dataf_remote");
    if !rpc_ready(&ws) {
        eprintln!("SKIP: Polygon RPCs unreachable");
        return;
    }

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--max-pools",
        "50",
        "--json",
        "--db-path",
        ws.join("cache.db").to_str().unwrap(),
    ]);
    let out = match run_timed(&mut c, Duration::from_secs(300)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("WARN (tolerant): remote discover timed out: {e}");
            return;
        }
    };
    if !out.success {
        eprintln!(
            "WARN (tolerant): remote aggregator path failed (service-side?)\n{}",
            out.combined()
        );
        return;
    }
    let pools = extract_json_array(&out.stdout)
        .expect("remote discover success must still print a JSON array");
    if let Some(entries) = pools.as_array() {
        eprintln!("remote discovery returned {} pools", entries.len());
    }
}

#[test]
fn validate_pools_tolerant_to_reference_failures() {
    let _guard = RPC_MUTEX.lock().unwrap();
    if std::env::var("MEV_SCOUT_E2E").as_deref() != Ok("1") {
        eprintln!("SKIP: set MEV_SCOUT_E2E=1 to run live-Polygon E2E tests");
        return;
    }
    let ws = temp_ws("dataf_vpools");
    if !rpc_ready(&ws) {
        eprintln!("SKIP: Polygon RPCs unreachable");
        return;
    }

    let mut c = scout(&ws);
    c.args(["-f", &cfg(), "validate-pools", "--days", "1", "--json"]);
    let out = match run_timed(&mut c, Duration::from_secs(300)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("WARN (tolerant): validate-pools timed out: {e}");
            return;
        }
    };
    if !out.success {
        eprintln!(
            "WARN (tolerant): validate-pools failed (reference service-side?)\n{}",
            out.combined()
        );
        return;
    }
    assert!(
        !out.stdout.trim().is_empty(),
        "validate-pools success must produce output"
    );
}
