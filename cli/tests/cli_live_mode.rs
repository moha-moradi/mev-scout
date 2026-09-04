mod common;

use common::{
    ensure_gate_and_rpc, expect_fail, expect_ok, repo_config, run_timed, scout, temp_config,
    temp_ws, HEAVY_TIMEOUT, RPC_MUTEX,
};
use serde_json::Value;
use std::time::Duration;

fn cfg() -> String {
    repo_config().to_str().unwrap().to_string()
}

fn live_cfg(ws: &std::path::Path) -> String {
    temp_config(
        ws,
        &[
            ("priority_fee_gwei", "30"),
            ("min_profit_wei", "0"),
            ("output", "\"json\""),
        ],
    )
    .to_str()
    .unwrap()
    .to_string()
}

fn newest_live_json(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("live_") && name.ends_with(".json") {
            let mtime = e.metadata().ok()?.modified().ok()?;
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[test]
fn live_one_shot_smoke() {
    let Some(ws) = ensure_gate_and_rpc("live1") else {
        return;
    };
    let _guard = RPC_MUTEX.lock().unwrap();
    let results = ws.join("results");
    let results_s = results.to_str().unwrap();
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let cfg = live_cfg(&ws);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "live",
        "--export-path",
        results_s,
        "--db-path",
        db_s,
    ]);
    let out = run_timed(&mut c, HEAVY_TIMEOUT).expect("live one-shot spawn failed");
    expect_ok(&out, "live one-shot");

    assert!(
        out.stdout.contains("Latest block:"),
        "missing tip line:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("opportunity(ies) detected"),
        "missing per-block detection summary:\n{}",
        out.stdout
    );

    let file = newest_live_json(&results).expect("live must export live_*.json");
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).expect("live_*.json parses");
    assert_eq!(parsed["range_mode"].as_str(), Some("live"));
    assert_eq!(parsed["chain"].as_str(), Some("polygon"));
    assert!(parsed["opportunities"].is_array());
}

#[test]
fn live_loop_duration_graceful_exit() {
    let Some(ws) = ensure_gate_and_rpc("loop30") else {
        return;
    };
    let _guard = RPC_MUTEX.lock().unwrap();
    let results = ws.join("results");
    let results_s = results.to_str().unwrap();
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let pipeline = live_cfg(&ws);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &pipeline,
        "live",
        "--loop",
        "--duration",
        "30s",
        "--poll-interval",
        "1000",
        "--export-path",
        results_s,
        "--db-path",
        db_s,
    ]);
    let out = run_timed(&mut c, Duration::from_secs(300)).expect("live loop spawn failed");
    expect_ok(
        &out,
        "live --loop --duration 30s must exit cleanly on its own before the harness timeout",
    );

    let combined = out.combined();
    assert!(
        combined.contains("Session summary:"),
        "duration-based exit must print a session summary:\n{combined}"
    );
    let blocks_line = combined
        .lines()
        .find(|l| l.trim_start().starts_with("Blocks processed:"))
        .expect("summary lists blocks processed");
    let blocks: u64 = blocks_line
        .split(':')
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .expect("blocks processed is numeric");
    assert!(blocks >= 1, "30s window on Polygon should process >=1 block");

    for forbidden in ["Fetch failed", "Backtest failed", "giving up"] {
        assert!(
            !combined.contains(forbidden),
            "contiguity violated: '{forbidden}' appeared\n{combined}"
        );
    }

    let exported = std::fs::read_dir(&results)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("live_") && n.ends_with(".json")
                })
                .count()
        })
        .unwrap_or(0);
    assert!(
        exported >= 1,
        "expected at least one live_*.json export per processed tick"
    );
}

#[test]
fn live_duration_without_loop_rejected_offline() {
    let ws = temp_ws("live_gate_offline");
    let mut c = scout(&ws);
    c.args(["-f", &cfg(), "live", "--duration", "15m"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("spawn failed");
    expect_fail(&out, "live --duration without --loop");
    assert!(out.stderr.contains("--duration requires --loop"));
}
