mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, extract_json_array, make_cfg, rpc_lock, run_timed, scout,
    HEAVY_TIMEOUT, NETWORK_TIMEOUT,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

fn append_toml(path: &str, extra: &str) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "\n{extra}").unwrap();
}

#[test]
fn data_foundation_pipeline_discover_tokens() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf") else {
        return;
    };
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let discover_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &discover_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "5",
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
    assert!(db.exists(), "sqlite db should exist after discover/tokens");
}

#[test]
fn discover_remote_tolerant_to_service_failures() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("dataf_remote") else {
        return;
    };

    let rem_cfg = make_cfg(
        &ws,
        &[
            ("db_path", ws.join("cache.db").to_str().unwrap()),
            ("output", "\"json\""),
        ],
    );
    append_toml(&rem_cfg, "[discover]\nmax_pools = 50");
    let mut c = scout(&ws);
    c.args([
        "-f",
        &rem_cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
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
