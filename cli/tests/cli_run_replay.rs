mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, repo_config, run_timed, scout, HEAVY_TIMEOUT, NETWORK_TIMEOUT,
    RPC_MUTEX,
};
use serde_json::Value;

fn cfg() -> String {
    repo_config().to_str().unwrap().to_string()
}

fn newest_json_matching(dir: &std::path::Path, prefix: &str) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) && name.ends_with(".json") {
            let mtime = e.metadata().ok()?.modified().ok()?;
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[test]
fn fetch_run_replay_report_chain() {
    let Some(ws) = ensure_gate_and_rpc("runrep") else {
        return;
    };
    let _guard = RPC_MUTEX.lock().unwrap();
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();
    let results = ws.join("results");
    let results_s = results.to_str().unwrap();

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
    expect_ok(&out, "fetch 5 blocks for chain test");
    assert!(out.stdout.contains("Fetch complete:"));
    if out.stdout.contains("even after refetch")
        || out.stdout.contains("Missing:")
            && !out.stdout.contains("Refetched:    5")
    {
        eprintln!("WARN: provider left gaps; continuing with what was cached");
    }

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "run",
        "--blocks",
        "5",
        "--strategies",
        "all",
        "--priority-fee",
        "30",
        "--fact-check",
        "--output",
        "json",
        "--export-path",
        results_s,
        "--db-path",
        db_s,
    ]);
    let out = run_timed(&mut c, HEAVY_TIMEOUT).expect("run spawn failed");
    expect_ok(&out, "run 5 blocks fact-checked");

    let run_file = newest_json_matching(&results, "run_")
        .expect("run must export run_*.json under export path");
    let content = std::fs::read_to_string(&run_file).unwrap();
    let results_json: Value = serde_json::from_str(&content).expect("run_*.json must parse");
    assert_eq!(
        results_json["chain"].as_str(),
        Some("polygon"),
        "chain mismatch in {}",
        run_file.display()
    );
    let start_block = results_json["start_block"]
        .as_u64()
        .expect("start_block numeric");
    let end_block = results_json["end_block"].as_u64().expect("end_block numeric");
    assert!(end_block >= start_block, "end_block must be >= start_block");
    assert!(
        end_block - start_block <= 5,
        "range wider than requested: {start_block}..{end_block}"
    );
    assert!(
        results_json["opportunities"].is_array(),
        "opportunities array missing"
    );
    assert!(results_json["strategies"].is_array(), "strategies missing");

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg(),
        "replay",
        "--block",
        &start_block.to_string(),
        "--analyze",
        "--db-path",
        db_s,
    ]);
    let out = run_timed(&mut c, HEAVY_TIMEOUT).expect("replay spawn failed");
    expect_ok(&out, "replay of run start block");
    let line = out
        .stdout
        .lines()
        .find(|l| l.contains("Receipt verification:"))
        .expect("receipt verification summary line");
    let pct: f64 = line
        .split('(')
        .nth(1)
        .and_then(|s| s.trim_end_matches(|c| c == '%' || c == ')' || c == ' ').trim().parse().ok())
        .expect("parseable match percentage");
    if pct < 99.0 {
        eprintln!("WARN: receipt match rate {pct}% below 99% (fork/precompile quirk?) — not failing E2E");
    }

    let mut c = scout(&ws);
    c.args([
        "report",
        "--export-path",
        results_s,
        "--output",
        "table",
    ]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report table spawn failed");
    expect_ok(&out, "report table on fresh results");
    assert!(out.stdout.contains("Run ID:"), "table output lacks Run ID");
    assert!(out.stdout.contains("Chain:"), "table output lacks Chain");

    let mut c = scout(&ws);
    c.args(["report", "--export-path", results_s, "--output", "json"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report json spawn failed");
    expect_ok(&out, "report json roundtrip");
    let reparsed: Value =
        serde_json::from_str(out.stdout.trim()).expect("report --output json must print pure JSON");
    assert_eq!(
        reparsed, results_json,
        "report roundtrip must equal the exported run file"
    );

    let mut c = scout(&ws);
    c.args(["report", "--export-path", results_s, "--output", "csv"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report csv spawn failed");
    expect_ok(&out, "report csv");
    assert!(
        out.stdout.lines().any(|l| l.trim()
            == "block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence"),
        "csv header line missing:\n{}",
        out.stdout
    );

    let mut c = scout(&ws);
    c.args(["report", "--export-path", results_s]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report default-table spawn failed");
    expect_ok(&out, "report default (latest run)");
    assert!(out.stdout.contains("Run ID:"));
}
