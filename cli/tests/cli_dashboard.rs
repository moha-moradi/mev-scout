//! Offline `explorer` dashboard CLI contract: config-owned DB, HTML-only output.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TimedOutput, BIN};
use rusqlite::Connection;

fn run(cwd: &Path, args: &[&str]) -> TimedOutput {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(cwd);
    cmd.args(args);
    let out = cmd.output().expect("spawn");
    TimedOutput {
        success: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn seed_db(path: &PathBuf) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("dir");
    }
    let _ = fs::remove_file(path);
    // Minimal v0-compatible file the dashboard can still read (never migrates).
    let conn = Connection::open(path).expect("open");
    conn.execute_batch(
        "CREATE TABLE blocks_classified(
           block INTEGER PRIMARY KEY, classified_at INTEGER NOT NULL, event_count INTEGER NOT NULL);
         CREATE TABLE mev_ops(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           block_number INTEGER NOT NULL, tx_index INTEGER, tx_hash TEXT NOT NULL,
           ts INTEGER NOT NULL, kind TEXT NOT NULL, eoa TEXT NOT NULL, contract TEXT,
           confidence TEXT NOT NULL, canonical_id TEXT, profit_token TEXT, profit_amount TEXT,
           profit_usd REAL, gas_cost_usd REAL, net_profit_usd REAL, route_json TEXT,
           victim_hashes TEXT, details_json TEXT, detector TEXT NOT NULL, created_at INTEGER NOT NULL);
         CREATE TABLE blocks(block_number INTEGER PRIMARY KEY, ts INTEGER NOT NULL, producer TEXT);
         CREATE TABLE sync_state(chain_id INTEGER PRIMARY KEY, head INTEGER, indexed_to INTEGER, last_indexed_at INTEGER);
         CREATE TABLE rejected_candidates(
           run_id TEXT, chain TEXT, block_number INTEGER NOT NULL, tx_index INTEGER,
           strategy TEXT NOT NULL, reject_reason TEXT NOT NULL, detail TEXT, created_at INTEGER NOT NULL);
         INSERT INTO sync_state VALUES(43114, 9, 9, 100);
         INSERT INTO blocks VALUES(1, 1700000000, NULL);
         INSERT INTO mev_ops(
           block_number, tx_index, tx_hash, ts, kind, eoa, confidence, profit_usd, gas_cost_usd,
           net_profit_usd, detector, created_at
         ) VALUES(1, 0, '0xcli', 1700000000, 'arb_atomic', '0xbbb', 'exact', 3.0, 0.3, 2.7, 'test', 1700000000);
         PRAGMA user_version = 0;",
    )
    .expect("seed");
}

/// Single-chain config pointing `[explorer] db_path` at the seeded fixture.
fn write_cfg(ws: &Path, db: &Path) -> String {
    let db = db.to_str().expect("utf8").replace('\\', "/");
    let cfg = ws.join("cfg.toml");
    fs::write(
        &cfg,
        format!("chain = \"avalanche\"\n\n[explorer]\ndb_path = \"{db}\"\n"),
    )
    .expect("cfg");
    cfg.to_str().expect("utf8").to_string()
}

#[test]
fn explorer_bare_writes_html_from_config_db() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(ws.join("cache")).expect("ws");
    let db = ws.join("cache").join("explorer-avalanche.sqlite");
    seed_db(&db);
    let before = fs::read(&db).expect("before");
    let cfg = write_cfg(&ws, &db);
    let out = ws.join("out").join("dash.html");

    let r = run(
        &ws,
        &["-f", &cfg, "explorer", "--out", out.to_str().expect("utf8")],
    );
    assert!(r.success, "stderr={}", r.combined());
    assert!(r.stdout.contains("dashboard written"), "{}", r.stdout);
    assert!(out.exists(), "html missing");
    let html = fs::read_to_string(&out).expect("read html");
    assert!(html.contains("id=\"snapshot\""));
    assert!(html.contains("id=\"nav\""));
    assert!(html.contains("id=\"view-overview\""));
    assert!(html.contains("id=\"view-operations\""));
    assert!(html.contains("id=\"op-export\""));
    assert!(html.contains("Export tx JSON"));
    assert!(html.contains("id=\"view-searchers\""));
    assert!(html.contains("id=\"view-pools\""));
    assert!(html.contains("id=\"view-analytics\""));
    assert!(html.contains("id=\"view-health\""));
    assert!(html.contains("Operations"));
    assert!(html.contains("Overview"));
    assert!(!html.contains("Network overview"));
    assert!(!html.contains("id=\"filter-bar\""));
    assert!(!html.contains("kpi-spark"));
    assert!(!html.contains("Fraunces"));
    assert!(!html.contains("#F7F4EF"));
    assert!(html.contains("#E84142"));
    assert!(html.contains("avalanche") || html.contains("explorer-avalanche"));
    assert!(!html.contains("</script><script>alert"));
    assert_eq!(fs::read(&db).expect("after"), before, "db mutated");
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn explorer_bare_default_output_name_tracks_chain_and_range() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_name_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(ws.join("cache")).expect("ws");
    let db = ws.join("cache").join("explorer-avalanche.sqlite");
    seed_db(&db);
    let cfg = write_cfg(&ws, &db);

    let r = run(&ws, &["-f", &cfg, "explorer"]);
    assert!(r.success, "stderr={}", r.combined());
    let out = ws.join("results").join("explorer-avalanche-all.html");
    assert!(out.exists(), "default output missing: {}", out.display());
    assert!(
        r.stdout.contains("explorer-avalanche-all.html"),
        "{}",
        r.stdout
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn dashboard_help_lists_retained_flags_only() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_help_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(&ws).expect("ws");
    let r = run(&ws, &["explorer", "--help"]);
    assert!(r.success, "{}", r.combined());
    let text = r.combined();
    assert!(text.contains("--out"), "missing retained flag --out");
    assert!(text.contains("index"), "missing index subcommand");
    for removed in [
        "--window",
        "--with-gaps",
        "--max-ops",
        "--evidence",
        "--budget-mb",
        "--dashboard",
        "--db",
        "--format",
        "--repeat",
        "--kind",
        "--confidence",
    ] {
        assert!(
            !text.contains(removed),
            "help still lists removed flag {removed}"
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn index_help_shows_range_and_out_flags() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_help_idx_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(&ws).expect("ws");
    let r = run(&ws, &["explorer", "index", "--help"]);
    assert!(r.success, "{}", r.combined());
    let text = r.combined();
    for kept in ["--window", "--from-block", "--to-block", "--out"] {
        assert!(text.contains(kept), "missing retained flag {kept}");
    }
    assert!(
        !text.contains("--dashboard"),
        "help still lists removed flag --dashboard"
    );
    let _ = fs::remove_dir_all(&ws);
}
