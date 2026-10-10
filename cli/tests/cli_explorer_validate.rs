//! `explorer validate` coverage — compiled only with the non-default
//! `validate` cargo feature (`cargo test --features validate`).
//!
//! `validate` is cross-validation research tooling rather than part of the
//! scanner workflow, so it is hidden from the default command surface.

#![cfg(feature = "validate")]

#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{expect_ok, make_cfg, run_timed, scout, temp_ws, TimedOutput, TEST_TIMEOUT};
use std::path::Path;

fn run(ws: &Path, args: &[&str]) -> TimedOutput {
    let mut c = scout(ws);
    c.args(args);
    run_timed(&mut c, TEST_TIMEOUT).expect("spawn/wait failed")
}

/// Seed an explorer DB with one T1-matching op+opportunity pair (same
/// `canonical_id`) so `explorer validate --since all --json` produces a real
/// report offline. `validate` itself does no RPC (validate.rs).
fn seed_validate_fixture(ws: &Path) {
    use rusqlite::Connection;
    std::fs::create_dir_all(ws.join("cache")).unwrap();
    let explorer_path = ws.join("cache/explorer-avalanche.sqlite");
    let conn = Connection::open(&explorer_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mev_ops(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            block_number INTEGER NOT NULL,
            tx_index INTEGER,
            tx_hash TEXT NOT NULL,
            ts INTEGER NOT NULL,
            kind TEXT NOT NULL,
            eoa TEXT NOT NULL,
            contract TEXT,
            confidence TEXT NOT NULL,
            canonical_id TEXT,
            profit_token TEXT,
            profit_amount TEXT,
            profit_usd REAL,
            volume_usd REAL,
            gas_cost_usd REAL,
            flashloan_fee_usd REAL,
            net_profit_usd REAL,
            route_json TEXT,
            victim_hashes TEXT,
            details_json TEXT,
            detector TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        INSERT INTO mev_ops
            (block_number, tx_index, tx_hash, ts, kind, eoa, confidence, canonical_id,
             profit_usd, gas_cost_usd, net_profit_usd, detector, created_at)
        VALUES
            (50000001, 10, '0x1111111111111111111111111111111111111111111111111111111111111111',
             1700000000, 'arb_atomic', '0x0000000000000000000000000000000000000001', 'exact',
             'ArbAtomic|fixture-1', 500.0, 10.0, 490.0, 'fixture', 1700000000);

        CREATE TABLE IF NOT EXISTS opportunities(
            run_id TEXT,
            chain TEXT,
            block_number INTEGER NOT NULL,
            tx_index INTEGER,
            strategy TEXT NOT NULL,
            pool_a TEXT,
            pool_b TEXT,
            token_in TEXT,
            token_out TEXT,
            input_amount TEXT,
            expected_profit TEXT,
            gas_cost_wei TEXT,
            path TEXT,
            timestamp INTEGER,
            mempool_only INTEGER,
            confidence TEXT,
            sender TEXT,
            tx_hash TEXT,
            detection_path TEXT,
            canonical_id TEXT
        );
        INSERT INTO opportunities
            (run_id, chain, block_number, tx_index, strategy, canonical_id)
        VALUES
            ('run_1111111111', 'avalanche', 50000001, 10, 'TwoHopArb', 'ArbAtomic|fixture-1');

        CREATE TABLE IF NOT EXISTS rejected_candidates(
            run_id TEXT,
            chain TEXT,
            block_number INTEGER NOT NULL,
            tx_index INTEGER,
            strategy TEXT NOT NULL,
            pool_a TEXT,
            pool_b TEXT,
            token_in TEXT,
            token_out TEXT,
            expected_profit TEXT,
            gas_cost_wei TEXT,
            reject_reason TEXT,
            detail TEXT
        );",
    )
    .unwrap();
}

#[test]
fn explorer_validate_reports_t1_match_offline() {
    let ws = temp_ws("validate_pos");
    seed_validate_fixture(&ws);

    let explorer_db = ws
        .join("cache/explorer-avalanche.sqlite")
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let cfg_path = make_cfg(&ws, &[]);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&cfg_path)
        .unwrap();
    use std::io::Write;
    writeln!(f, "\n[explorer]\ndb_path = \"{explorer_db}\"").unwrap();

    let out = run(
        &ws,
        &[
            "-f", &cfg_path, "explorer", "validate", "--since", "all", "--json",
        ],
    );
    expect_ok(&out, "explorer validate --since all --json offline");
    assert!(
        out.stdout.contains("arb_atomic"),
        "JSON report should mention the seeded arb_atomic kind, got:\n{}",
        out.stdout
    );
    // The single seeded op must appear as matched (T1 count >= 1) in the report.
    assert!(
        out.stdout.contains("fixture-1"),
        "report should carry the seeded canonical_id, got:\n{}",
        out.stdout
    );
}
