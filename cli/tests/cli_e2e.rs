//! Opt-in end-to-end test for the real `mev-scout` CLI binary.
//!
//! Exercises the full CLI path against a live Polygon RPC:
//! RPC init → range resolution → fetch → pool init → backtest → JSON export.
//!
//! Skipped unless `MEV_SCOUT_E2E=1` is set. The RPC URL is read from the
//! `rpc_urls[0]` entry of `mev-scout.toml` at the workspace root (no env var
//! required; override with `RPC_URL` if you want a different endpoint).

mod common;

use common::{first_rpc_url, run_timed, temp_config, temp_ws, HEAVY_TIMEOUT};
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

    let rpc = match std::env::var("RPC_URL") {
        Ok(url) => url,
        Err(_) => match first_rpc_url() {
            Some(url) => url,
            None => {
                skip("could not read first RPC URL from mev-scout.toml; set RPC_URL to override");
                return;
            }
        },
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
            ("export_path", export.to_str().unwrap()),
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

    // The CLI writes results to <export>/run_<epoch>.json.
    let results: Vec<_> = std::fs::read_dir(&export)
        .unwrap_or_else(|e| panic!("failed to read export dir {}: {e}", export.display()))
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("run_") && name.ends_with(".json")
        })
        .collect();
    assert!(
        !results.is_empty(),
        "expected run_*.json results in {}",
        export.display()
    );

    for r in &results {
        let content = std::fs::read_to_string(r.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["chain"], "polygon",
            "results file should report chain=polygon"
        );
        assert!(
            parsed["start_block"].is_number() && parsed["end_block"].is_number(),
            "results file should include a numeric block range"
        );
    }

    let _ = std::fs::remove_dir_all(&export);
}
