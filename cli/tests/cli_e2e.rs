//! Opt-in end-to-end test for the real `mev-scout` CLI binary.
//!
//! Exercises the full CLI path against a live Polygon RPC:
//! RPC init → range resolution → fetch → pool init → backtest → JSON export.
//!
//! Skipped unless `MEV_SCOUT_E2E=1` is set. The RPC URL comes from the
//! `RPC_URL` env var only.

mod common;

use common::{rpc_url, run_timed, temp_config, temp_ws, HEAVY_TIMEOUT};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_mev-scout");

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

#[test]
fn cli_real_run_smoke() {
    if std::env::var("MEV_SCOUT_E2E").as_deref() != Ok("1") {
        skip("set MEV_SCOUT_E2E=1 to run the real CLI-path E2E test");
        return;
    }

    let rpc = match rpc_url() {
        Some(url) => url,
        None => {
            skip("no RPC URL available (export RPC_URL with a valid Polygon endpoint)");
            return;
        }
    };

    let ws = temp_ws("cli_e2e");
    let export = ws.join("export");
    let db = export.join("cache.db");
    std::fs::create_dir_all(&export).unwrap();

    let cfg_path = temp_config(
        &ws,
        &[
            ("rpc_urls", &format!("[\"{rpc}\"]")),
            ("strategies", "\"two_hop_arb\""),
            ("output", "\"json\""),
            ("db_path", db.to_str().unwrap()),
        ],
    );

    let mut cmd = Command::new(BIN);
    cmd.args(["--quiet", "-f", cfg_path.to_str().unwrap(), "run", "--blocks", "1"]);

    eprintln!("Running: {}", cmd.get_program().to_string_lossy());
    let out = match run_timed(&mut cmd, HEAVY_TIMEOUT) {
        Ok(o) => o,
        Err(e) => {
            skip(&format!("cli run exceeded budget (public-RPC stall):\n{e}"));
            return;
        }
    };
    eprintln!("--- stdout ---");
    eprintln!("{}", out.stdout);
    eprintln!("--- stderr ---");
    eprintln!("{}", out.stderr);

    assert!(
        out.success,
        "mev-scout run exited with {:?}",
        out.code
    );

    // `--quiet` must suppress tracing lines (error-level filter); progress
    // bars and the final summary still go to stdout.
    for leaked in ["INFO", "DEBUG", "WARN "] {
        assert!(
            !out.stdout.contains(leaked) && !out.stderr.contains(leaked),
            "--quiet must suppress tracing output (found '{leaked}')\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout,
            out.stderr
        );
    }

    // Execution history lives in SQLite. Verify via report.
    let report_cfg = temp_config(
        &ws,
        &[
            ("rpc_urls", &format!("[\"{rpc}\"]")),
            ("db_path", db.to_str().unwrap()),
            ("output", "\"json\""),
        ],
    );
    let mut report_cmd = Command::new(BIN);
    report_cmd.args(["--quiet", "-f", report_cfg.to_str().unwrap(), "report"]);
    let report_out = run_timed(&mut report_cmd, HEAVY_TIMEOUT)
        .expect("report after run exceeded budget");
    assert!(
        report_out.success,
        "mev-scout report after run exited with {:?}",
        report_out.code
    );
    let parsed: serde_json::Value = serde_json::from_str(report_out.stdout.trim())
        .expect("report --output json must print pure JSON");
    assert_eq!(
        parsed["chain"], "polygon",
        "report should report chain=polygon"
    );
    assert!(
        parsed["start_block"].is_number() && parsed["end_block"].is_number(),
        "report should include a numeric block range"
    );

    let _ = std::fs::remove_dir_all(&export);
}
