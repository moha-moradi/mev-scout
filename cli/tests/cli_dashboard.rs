//! Offline `explorer dashboard` CLI contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use common::{TimedOutput, BIN};
use rusqlite::Connection;

fn run(args: &[&str]) -> TimedOutput {
    let mut cmd = Command::new(BIN);
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
    // Create via the library so schema is at head, then reopen to insert.
    // Integration test only has rusqlite — build a minimal head-compatible file
    // by invoking the binary's sibling approach: create empty then let open migrate
    // isn't available. Seed a v0-compatible file the dashboard can still read.
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

#[test]
fn dashboard_html_offline_no_config() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(ws.join("cache")).expect("ws");
    let db = ws.join("cache").join("explorer-avalanche.sqlite");
    seed_db(&db);
    let before = fs::read(&db).expect("before");
    let out = ws.join("dash.html");

    let r = run(&[
        "explorer",
        "dashboard",
        "--db",
        db.to_str().expect("utf8"),
        "--window",
        "all",
        "--out",
        out.to_str().expect("utf8"),
        "--format",
        "html",
    ]);
    assert!(r.success, "stderr={}", r.combined());
    assert!(out.exists(), "html missing");
    let html = fs::read_to_string(&out).expect("read html");
    assert!(html.contains("id=\"snapshot\""));
    assert!(html.contains("id=\"hero\""));
    assert!(html.contains("id=\"live\""));
    assert!(html.contains("id=\"activity\""));
    assert!(html.contains("id=\"pools\""));
    assert!(html.contains("id=\"searchers\""));
    assert!(html.contains("id=\"details\""));
    assert!(html.contains("Fraunces"));
    assert!(html.contains("#F7F4EF"));
    assert!(html.contains("#E84142"));
    assert!(!html.contains("id=\"nav\""));
    assert!(html.contains("avalanche") || html.contains("explorer-avalanche"));
    assert!(!html.contains("</script><script>alert"));
    assert_eq!(fs::read(&db).expect("after"), before, "db mutated");
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn dashboard_csv_offline_report_header() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_csv_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(ws.join("cache")).expect("ws");
    let db = ws.join("cache").join("explorer-avalanche.sqlite");
    seed_db(&db);
    let out = ws.join("dash.csv");

    let r = run(&[
        "explorer",
        "dashboard",
        "--db",
        db.to_str().expect("utf8"),
        "--window",
        "all",
        "--out",
        out.to_str().expect("utf8"),
        "--format",
        "csv",
    ]);
    assert!(r.success, "{}", r.combined());
    let csv = fs::read_to_string(&out).expect("csv");
    assert!(csv.starts_with(
        "window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd"
    ));
    assert!(csv.contains(",overview,total,"));
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn dashboard_json_offline() {
    let ws = std::env::temp_dir().join(format!("mev_cli_dash_json_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(ws.join("cache")).expect("ws");
    let db = ws.join("cache").join("explorer-avalanche.sqlite");
    seed_db(&db);
    let out = ws.join("dash.json");

    let r = run(&[
        "explorer",
        "dashboard",
        "--db",
        db.to_str().expect("utf8"),
        "--window",
        "all",
        "--out",
        out.to_str().expect("utf8"),
        "--format",
        "json",
    ]);
    assert!(r.success, "{}", r.combined());
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).expect("json")).expect("parse");
    assert_eq!(v["api_version"], 1);
    assert_eq!(v["db"]["user_version"], 0);
    assert_eq!(v["coverage"]["ops_in_window"], 1);
    assert_eq!(v["panels"]["kpi"]["ops"], 1);
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn dashboard_help_lists_flags() {
    let r = run(&["explorer", "dashboard", "--help"]);
    assert!(r.success, "{}", r.combined());
    let text = r.combined();
    assert!(text.contains("--db"));
    assert!(text.contains("--window"));
    assert!(text.contains("--format"));
    assert!(text.contains("--with-gaps"));
    assert!(text.contains("--repeat"));
}
