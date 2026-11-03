//! Guard: dashboard path must never migrate or mutate the explorer DB file.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use mev_scout_core::explorer::dashboard::caps::StoreCaps;
use mev_scout_core::explorer::dashboard::{build, BuildOptions, WindowSpec};
use mev_scout_core::explorer::store::{ExplorerStore, SCHEMA_HEAD};

fn sha256_file(path: &Path) -> anyhow::Result<[u8; 32]> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

fn temp_db(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mev_dash_ro_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(e) = fs::create_dir_all(&dir) {
        panic!("tmpdir: {e}");
    }
    dir.join("explorer-avalanche.sqlite")
}

#[test]
fn open_read_only_does_not_bump_user_version_on_legacy() {
    let path = temp_db("legacy");
    {
        let conn = Connection::open(&path).expect("create");
        conn.execute_batch(
            "CREATE TABLE blocks_classified(
               block INTEGER PRIMARY KEY,
               classified_at INTEGER NOT NULL,
               event_count INTEGER NOT NULL);
             CREATE TABLE mev_ops(
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
               gas_cost_usd REAL,
               net_profit_usd REAL,
               route_json TEXT,
               victim_hashes TEXT,
               details_json TEXT,
               detector TEXT NOT NULL,
               created_at INTEGER NOT NULL);
             CREATE TABLE blocks(block_number INTEGER PRIMARY KEY, ts INTEGER NOT NULL, producer TEXT);
             CREATE TABLE sync_state(chain_id INTEGER PRIMARY KEY, head INTEGER, indexed_to INTEGER, last_indexed_at INTEGER);
             INSERT INTO sync_state VALUES(43114, 1, 1, 100);
             PRAGMA user_version = 0;",
        )
        .expect("ddl");
    }
    let before_v: i64 = {
        let c = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("ro");
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("v")
    };
    assert_eq!(before_v, 0);

    let store = ExplorerStore::open_read_only(&path).expect("open_read_only");
    assert_eq!(store.user_version().expect("uv"), 0);

    // Probe via a separate read-only connection (store.connection is crate-private).
    {
        let c = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("probe conn");
        let caps = StoreCaps::probe(&c).expect("caps");
        assert!(!caps.has_subkind);
        assert!(!caps.has_excluded_from_net);
        assert_eq!(caps.status_label(), "legacy_unmigrated");
    }

    match store.set_sync_state(1, 1, 1) {
        Ok(()) => panic!("writes must fail on read-only"),
        Err(err) => {
            let msg = format!("{err:#}");
            assert!(
                msg.to_lowercase().contains("readonly")
                    || msg.contains("SQLITE_READONLY")
                    || msg.to_lowercase().contains("read-only"),
                "unexpected err: {msg}"
            );
        }
    }

    drop(store);
    let after_v: i64 = {
        let c = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("ro2");
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("v2")
    };
    assert_eq!(after_v, 0, "dashboard must not migrate");
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn dashboard_build_leaves_file_bytes_unchanged() {
    let path = temp_db("head");
    {
        let store = ExplorerStore::open(&path).expect("create head");
        store.set_sync_state(43114, 50, 50).expect("sync");
        drop(store);
        let conn = Connection::open(&path).expect("rw");
        conn.execute(
            "INSERT INTO blocks(block_number, block_hash, ts, indexed_at)
             VALUES(1, '0xbb', 1700000000, 1700000000)",
            [],
        )
        .expect("block");
        conn.execute(
            "INSERT INTO mev_ops(
               block_number, tx_index, tx_hash, ts, kind, eoa, confidence,
               profit_usd, gas_cost_usd, net_profit_usd, detector, created_at, subkind, excluded_from_net
             ) VALUES(1, 0, '0xabc', 1700000000, 'arb_atomic', '0xeee', 'exact',
               1.5, 0.1, 1.4, 'test', 1700000000, NULL, 0)",
            [],
        )
        .expect("op");
    }
    let before = sha256_file(&path).expect("sha1");
    let before_meta = fs::metadata(&path).expect("meta").len();

    let snap = build(&BuildOptions {
        db_path: path.clone(),
        window: WindowSpec::all(),
        max_ops: 100,
        max_evidence: 10,
        budget_mb: 2,
        with_gaps: false,
        gap_block_limit: 100,
    })
    .expect("build");
    assert_eq!(snap.db.user_version, SCHEMA_HEAD);
    assert_eq!(snap.coverage.ops_in_window, 1);
    assert!((snap.panels.kpi.net_usd - 1.4).abs() < 1e-9);
    assert!(!snap.gaps.enabled);

    let after = sha256_file(&path).expect("sha2");
    assert_eq!(before, after, "DB sha256 must be unchanged");
    assert_eq!(before_meta, fs::metadata(&path).expect("meta2").len());
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn missing_db_errors_with_path() {
    let path = temp_db("missing");
    let _ = fs::remove_file(&path);
    match ExplorerStore::open_read_only(&path) {
        Ok(_) => panic!("missing db must error"),
        Err(err) => {
            let msg = format!("{err:#}");
            assert!(msg.contains("not found"), "{msg}");
        }
    }
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn build_works_on_legacy_schema_v0() {
    let path = temp_db("legacy_build");
    {
        let conn = Connection::open(&path).expect("create");
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
             INSERT INTO sync_state VALUES(43114, 10, 10, 100);
             INSERT INTO blocks VALUES(1, 1700000000, NULL);
             INSERT INTO mev_ops(
               block_number, tx_index, tx_hash, ts, kind, eoa, confidence, profit_usd, gas_cost_usd,
               net_profit_usd, detector, created_at
             ) VALUES(1, 0, '0xlegacy', 1700000000, 'unknown', '0xaaa', 'exact', 2.0, 0.2, 1.8, 'test', 1700000000);
             PRAGMA user_version = 0;",
        )
        .expect("ddl");
    }
    let before = sha256_file(&path).expect("sha1");
    let snap = build(&BuildOptions {
        db_path: path.clone(),
        window: WindowSpec::all(),
        with_gaps: true,
        gap_block_limit: 100,
        ..BuildOptions::default()
    })
    .expect("build legacy");
    assert_eq!(snap.db.user_version, 0);
    assert_eq!(snap.db.status, "legacy_unmigrated");
    assert_eq!(snap.panels.unknown_rewritten, 1);
    assert!(!snap.caps.has_excluded_from_net);
    assert_eq!(snap.coverage.ops_in_window, 1);
    assert!(snap.gaps.enabled);
    assert_eq!(sha256_file(&path).expect("sha2"), before);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}
