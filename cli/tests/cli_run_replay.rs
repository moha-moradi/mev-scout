mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, make_cfg, rpc_lock, run_timed, scout, HEAVY_TIMEOUT,
};
use serde_json::Value;

#[test]
fn run_report_chain() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("runrep") else {
        return;
    };
    let db = ws.join("cache.db");
    let db_s = db.to_str().unwrap();

    let run_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &run_cfg, "run", "--blocks", "5"]);
    let out = run_timed(&mut c, HEAVY_TIMEOUT).expect("run spawn failed");
    expect_ok(&out, "run 5 blocks");

    // Read run history from SQLite via report
    let report_json_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &report_json_cfg, "report"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report json spawn failed");
    expect_ok(&out, "report json from sqlite");
    let results_json: Value =
        serde_json::from_str(out.stdout.trim()).expect("report --output json must print pure JSON");
    assert_eq!(
        results_json["chain"].as_str(),
        Some("polygon"),
        "chain mismatch in report"
    );
    let start_block = results_json["start_block"]
        .as_u64()
        .expect("start_block numeric");
    let end_block = results_json["end_block"]
        .as_u64()
        .expect("end_block numeric");
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

    let tab_cfg = make_cfg(&ws, &[("db_path", db_s)]);
    let mut c = scout(&ws);
    c.args(["-f", &tab_cfg, "report"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report table spawn failed");
    expect_ok(&out, "report table on fresh results");
    assert!(out.stdout.contains("Run ID:"), "table output lacks Run ID");
    assert!(out.stdout.contains("Chain:"), "table output lacks Chain");

    let csv_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"csv\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &csv_cfg, "report"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report csv spawn failed");
    expect_ok(&out, "report csv");
    assert!(
        out.stdout.lines().any(|l| {
            l.trim()
            == "block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence"
        }),
        "csv header line missing:\n{}",
        out.stdout
    );

    // Default (latest) report
    let tab_cfg2 = make_cfg(&ws, &[("db_path", db_s)]);
    let mut c = scout(&ws);
    c.args(["-f", &tab_cfg2, "report"]);
    let out = run_timed(&mut c, common::TEST_TIMEOUT).expect("report default-table spawn failed");
    expect_ok(&out, "report default (latest run)");
    assert!(out.stdout.contains("Run ID:"));
}
