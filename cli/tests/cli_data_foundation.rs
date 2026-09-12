mod common;

use common::{
    ensure_gate_and_rpc, example_config_str, expect_ok, extract_json_array, make_cfg, rpc_lock,
    run_timed, scout, HEAVY_TIMEOUT, NETWORK_TIMEOUT,
};
use std::time::Duration;

#[test]
fn data_foundation_pipeline_discover_tokens_fetch_scan() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf") else {
        return;
    };
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let discover_cfg = make_cfg(&ws, &[("db_path", db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &discover_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "5",
        "--json",
    ]);
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
    let entries = pools
        .as_array()
        .expect("discover --json output must be an array");
    for p in entries {
        assert!(p.get("address").is_some(), "pool missing address: {p}");
        assert!(p.get("token0").is_some(), "pool missing token0: {p}");
        assert!(p.get("token1").is_some(), "pool missing token1: {p}");
        assert!(p.get("dex_type").is_some(), "pool missing dex_type: {p}");
    }

    // tokens on the SHARED pipeline db so the cache handoff between stages is
    // real (scan uses no local db by design — it reads the chain live).
    let tokens_base_cfg = make_cfg(&ws, &[("db_path", db_s)]);
    let mut c = scout(&ws);
    c.args(["-f", &tokens_base_cfg, "tokens", "--cache-only"]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("tokens spawn failed");
    expect_ok(&out, "tokens --cache-only");
    assert!(
        out.stdout.contains("Token cache:"),
        "expected cache summary line, got: {}",
        out.stdout
    );

    let json_cfg = make_cfg(&ws, &[("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &json_cfg, "tokens"]);
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

    let csv_cfg = make_cfg(&ws, &[("output", "\"csv\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &csv_cfg, "tokens"]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("tokens csv spawn failed");
    expect_ok(&out, "tokens --output csv");
    assert!(
        out.stdout
            .lines()
            .any(|l| l.trim() == "address,symbol,decimals"),
        "csv header line missing:\n{}",
        out.stdout
    );

    let fetch_cfg = make_cfg(&ws, &[("db_path", db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &fetch_cfg,
        "fetch",
        "--blocks",
        "5",
        "--no-sig-resolve",
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

    let scan_cfg = make_cfg(&ws, &[("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f", &scan_cfg, "scan", "--kind", "trades", "--blocks", "5", "--limit", "20",
    ]);
    let out = run_timed(&mut c, NETWORK_TIMEOUT).expect("scan spawn failed");
    expect_ok(&out, "scan trades 5 blocks json");
    let events = extract_json_array(&out.stdout).expect("scan --output json should print array");
    let items = events
        .as_array()
        .expect("scan --output json must be an array");
    for e in items {
        assert!(e.get("block").is_some(), "trade event missing block: {e}");
        assert!(
            e.get("tx_hash").is_some(),
            "trade event missing tx_hash: {e}"
        );
    }
}

#[test]
fn discover_remote_tolerant_to_service_failures() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf_remote") else {
        return;
    };

    let rem_cfg = make_cfg(&ws, &[("db_path", ws.join("cache.db").to_str().unwrap())]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &rem_cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--max-pools",
        "50",
        "--json",
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
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf_vpools") else {
        return;
    };

    let mut c = scout(&ws);
    c.args([
        "-f",
        &example_config_str(),
        "validate-pools",
        "--days",
        "1",
        "--json",
    ]);
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
