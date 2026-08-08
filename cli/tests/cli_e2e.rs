//! Opt-in end-to-end test for the real `mev-scout` CLI binary.
//!
//! Exercises the full CLI path against a live Polygon RPC:
//! RPC init → range resolution → fetch → pool init → backtest → JSON export.
//!
//! Skipped unless BOTH are set:
//!   - `MEV_SCOUT_E2E=1`  (opt-in flag; keeps the default test run offline)
//!   - `RPC_URL`          (Polygon RPC endpoint)

use std::process::{Command, Stdio};

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
        Err(_) => {
            skip("RPC_URL must be set when MEV_SCOUT_E2E=1");
            return;
        }
    };

    let export = std::env::temp_dir().join(format!("mev_scout_cli_e2e_{}", std::process::id()));
    let db = export.join("cache.db");
    std::fs::create_dir_all(&export).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args([
        "--quiet",
        "run",
        "--blocks",
        "1",
        "--strategies",
        "two_hop_arb",
        "--output",
        "json",
        "--export-path",
        export.to_str().unwrap(),
        "--db-path",
        db.to_str().unwrap(),
        "--rpc",
        &rpc,
    ]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    eprintln!("Running: {}", cmd.get_program().to_string_lossy());
    let out = cmd.output().expect("failed to spawn mev-scout binary");
    eprintln!("--- stdout ---");
    eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    eprintln!("--- stderr ---");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));

    assert!(
        out.status.success(),
        "mev-scout run exited with {:?}",
        out.status
    );

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
