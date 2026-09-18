//! Shared test harness for the mev-scout-api integration tests.
//!
//! Two styles of `AppState`:
//! - [`test_state`] — in-memory DB connections (used by routes that read
//!   through `state.explorer_conn` / `state.cache_conn` directly).
//! - [`state_with_real_dbs`] — real temp **files** on disk (used by routes
//!   that open a fresh `ExplorerStore` / `SqliteStore` via the DB path:
//!   `/api/pools`, `/api/results/:id`, `/api/results/:id/validation`,
//!   `/api/results/:id/pnl`).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use rusqlite::Connection;

use mev_scout_core::types::ChainName;

use mev_scout_api::jobs::JobManager;
use mev_scout_api::state::{AppState, SharedState};
pub use mev_scout_api::test_router;

const EXPLORER_SCHEMA: &str = include_str!("../../src/explorer_schema.sql");
const CACHE_SCHEMA: &str = include_str!("../../src/cache_schema.sql");

/// Build a `SharedState` with in-memory explorer + cache DBs (seeded).
/// The tempdir backing the job manager is leaked (forgotten) so the log
/// dir outlives the helper for jobs tests.
pub async fn test_state() -> SharedState {
    state_with_conns(seeded_explorer_db(), seeded_cache_db()).await
}

/// Build a `SharedState` from explicit explorer/cache connections (used for
/// missing-DB and empty-result tests).
pub async fn state_with_conns(explorer_conn: Connection, cache_conn: Connection) -> SharedState {
    let data_dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
    let config = mev_scout_core::config::Config {
        chain: ChainName::Polygon,
        config_path: None,
        ..Default::default()
    };
    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config),
        config_path: PathBuf::from("mev-scout.toml"),
        explorer_db_path: tokio::sync::RwLock::new(PathBuf::from("nonexistent-explorer.sqlite")),
        cache_db_path: tokio::sync::RwLock::new(PathBuf::from("nonexistent-cache.sqlite")),
        explorer_conn: tokio::sync::Mutex::new(explorer_conn),
        cache_conn: tokio::sync::Mutex::new(cache_conn),
        job_manager: Arc::new(tokio::sync::Mutex::new(JobManager::new(data_dir.path()))),
        started_at: std::time::Instant::now(),
        version: "test",
    })
}

/// Build a `SharedState` whose config is loaded from a real temp file (for
/// `PUT /api/config` round-trip tests). Jobs spawned under this state run in
/// `MEV_SCOUT_JOB_STUB` mode (set here) so `/api/jobs*` tests exercise the
/// manager without a live RPC backend.
pub async fn test_state_with_files(config_path: PathBuf) -> SharedState {
    // The job executor checks this env var to route to the in-process stub.
    std::env::set_var("MEV_SCOUT_JOB_STUB", "1");

    let config = mev_scout_core::config::Config::load(&config_path.to_string_lossy())
        .unwrap_or_else(|_| mev_scout_core::config::Config::default());

    let explorer_conn = seeded_explorer_db();
    let cache_conn = seeded_cache_db();
    let data_dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config),
        config_path: config_path.clone(),
        explorer_db_path: tokio::sync::RwLock::new(PathBuf::from("nonexistent-explorer.sqlite")),
        cache_db_path: tokio::sync::RwLock::new(PathBuf::from("nonexistent-cache.sqlite")),
        explorer_conn: tokio::sync::Mutex::new(explorer_conn),
        cache_conn: tokio::sync::Mutex::new(cache_conn),
        job_manager: Arc::new(tokio::sync::Mutex::new(JobManager::new(data_dir.path()))),
        started_at: std::time::Instant::now(),
        version: "test",
    })
}

/// Build a `SharedState` backed by real temp DB **files** seeded with the
/// same rows as the in-memory variants. Needed by routes that open a fresh
/// store over the DB path (`pools`, `results/:id`, `validation`, `pnl`).
pub async fn state_with_real_dbs() -> SharedState {
    let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
    let explorer_path = dir.path().join("explorer-polygon.sqlite");
    let cache_path = dir.path().join("polygon-mev-scout.sqlite");

    {
        let conn = Connection::open(&explorer_path).unwrap();
        conn.execute_batch(EXPLORER_SCHEMA).unwrap();
        seed_explorer_rows(&conn);
    }
    {
        let conn = Connection::open(&cache_path).unwrap();
        conn.execute_batch(CACHE_SCHEMA).unwrap();
        seed_cache_rows(&conn);
    }

    let config = mev_scout_core::config::Config {
        chain: ChainName::Polygon,
        config_path: None,
        ..Default::default()
    };

    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config),
        config_path: PathBuf::from("mev-scout.toml"),
        explorer_db_path: tokio::sync::RwLock::new(explorer_path.clone()),
        cache_db_path: tokio::sync::RwLock::new(cache_path.clone()),
        explorer_conn: tokio::sync::Mutex::new(seeded_explorer_db()),
        cache_conn: tokio::sync::Mutex::new(seeded_cache_db()),
        job_manager: Arc::new(tokio::sync::Mutex::new(JobManager::new(dir.path()))),
        started_at: std::time::Instant::now(),
        version: "test",
    })
}

/// Build a router over the in-memory state (mounted at `/api`).
pub async fn test_router_state() -> Router<()> {
    test_router(test_state().await)
}

pub fn seeded_explorer_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(EXPLORER_SCHEMA).unwrap();
    seed_explorer_rows(&conn);
    conn
}

pub fn seeded_cache_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(CACHE_SCHEMA).unwrap();
    seed_cache_rows(&conn);
    conn
}

pub fn empty_explorer_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(EXPLORER_SCHEMA).unwrap();
    conn
}

/// Minimal rows so read endpoints return non-empty results.
fn seed_explorer_rows(conn: &Connection) {
    let now = mev_scout_core::utils::epoch_secs();

    conn.execute_batch(&format!(
        "INSERT INTO mev_ops
            (block_number, tx_index, tx_hash, ts, kind, eoa, contract, confidence,
             canonical_id, profit_token, profit_amount, profit_usd, gas_cost_usd,
             net_profit_usd, route_json, victim_hashes, details_json, detector, created_at)
         VALUES
            (100, 0, '0xAAA', {now}, 'sandwich', '0xS1', '0xC1', 'high',
             'c0', '0xT1', '1000000000000000000', 1.5, 0.2, 1.3, '[]', '[]', '{{}}', 'det1', {now}),
            (101, 1, '0xBBB', {now}, 'arb_atomic', '0xE2', '0xC2', 'medium',
             'c1', '0xT2', '500000000000000000', 0.8, 0.1, 0.7, '[]', '[]', '{{}}', 'det2', {now}),
            (202, 2, '0xCCC', {now}, 'liquidation', '0xS1', '0xC3', 'high',
             'c2', '0xT3', '250000000000000000', 2.0, 0.3, 1.7, '[]', '[]', '{{}}', 'det3', {now})
        "
    ))
    .unwrap();

    conn.execute_batch(&format!(
        "INSERT INTO opportunities
            (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b,
             token_in, token_out, input_amount, expected_profit, gas_cost_wei,
             path, \"timestamp\", mempool_only, confidence, sender, tx_hash,
             detection_path, canonical_id)
         VALUES
            ('run_1', 'polygon', 100, 0, 'two_hop_arb',
             '0x00000000000000000000000000000000000000b0',
             '0x00000000000000000000000000000000000000b1',
             '0x00000000000000000000000000000000000000a0',
             '0x3c499c542cef5e3811e1192ce70d8cc03d5c3359',
             '1000', '1200000000000000000', '21000',
             NULL, {now}, 0, 'high',
             '0x00000000000000000000000000000000000000e1',
             '0x1111111111111111111111111111111111111111111111111111111111111111',
             'atomic', NULL),
            ('run_1', 'polygon', 101, 1, 'two_hop_arb',
             '0x00000000000000000000000000000000000000b2',
             '0x00000000000000000000000000000000000000b3',
             '0x00000000000000000000000000000000000000a1',
             '0x00000000000000000000000000000000000000c2',
             '2000', '800000000000000000', '21000',
             NULL, {now}, 0, 'medium',
             '0x00000000000000000000000000000000000000e2',
             '0x2222222222222222222222222222222222222222222222222222222222222222',
             'atomic', NULL),
            ('live_1777', 'polygon', 202, 2, 'jit',
             '0x00000000000000000000000000000000000000b4',
             '0x00000000000000000000000000000000000000b5',
             '0x00000000000000000000000000000000000000a2',
             '0x00000000000000000000000000000000000000c3',
             '3000', '600000000000000000', '21000',
             NULL, {now}, 0, 'high',
             '0x00000000000000000000000000000000000000e3',
             '0x3333333333333333333333333333333333333333333333333333333333333333',
             'jit', NULL)
        "
    ))
    .unwrap();

    // A prices row so PnL can attempt USD conversion (token matches the
    // run_1 first-row token_out).
    let hour = now / 3600;
    conn.execute(
        "INSERT INTO prices (hour, token, usd, source) VALUES (?1, '0x3c499c542cef5e3811e1192ce70d8cc03d5c3359', 3.0, 'test')",
        rusqlite::params![hour],
    )
    .unwrap();
}

/// Minimal rows so results/runs/pools return non-empty results.
fn seed_cache_rows(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO run_manifests
            (run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider)
         VALUES
            ('run_1', 'polygon', 100, 101, 1700000000, 'Range', 'two_hop_arb,jit', 'auto'),
            ('run_2', 'polygon', 200, 202, 1699999999, 'Range', 'two_hop_arb', 'auto')
        ",
    )
    .unwrap();

    conn.execute_batch(
        "INSERT INTO pool_info
            (address, token0, token1, fee, dex_type, tick_spacing, creation_block,
             pool_id, factory, is_stable, underlying_tokens, balancer_pool_type,
             hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol,
             token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d)
         VALUES
            (X'0000000000000000000000000000000000000001',
             X'0000000000000000000000000000000000000002',
             X'0000000000000000000000000000000000000003',
             3000, 0, 60, 100,
             NULL, NULL, 0, NULL, NULL, NULL, NULL, NULL,
             'uniswap-v3', 'WETH', 'USDC', 500000.0, 10000.0, 200000.0),
            (X'0000000000000000000000000000000000000004',
             X'0000000000000000000000000000000000000005',
             X'0000000000000000000000000000000000000006',
             500, 0, 1, 200,
             NULL, NULL, 0, NULL, NULL, NULL, NULL, NULL,
             'pancakeswap', 'USDC', 'USDT', 300000.0, 8000.0, 90000.0),
            (X'0000000000000000000000000000000000000007',
             X'0000000000000000000000000000000000000008',
             X'0000000000000000000000000000000000000009',
             100, 1, NULL, 300,
             NULL, NULL, 1, NULL, NULL, NULL, NULL, NULL,
             'balancer', 'DAI', 'USDC', NULL, NULL, NULL)
        ",
    )
    .unwrap();
}

/// Write a temp config file with known secrets and return its path.
/// Uses the flat key format of `mev-scout.example.toml` (sub-configs are
/// `#[serde(flatten)]`ed, so `gas`/`backtest`/`output`/`rpc` fields live at
/// the top level). Includes `[chains.*]` sections for the chains the tests
/// switch between (required by core validation's `resolve_chain`).
pub fn write_temp_config(dir: &Path, chain: &str) -> PathBuf {
    let path = dir.join("mev-scout.toml");
    std::fs::write(
        &path,
        format!(
            r#"
chain = "{chain}"
gas_model = "historical_exact"
gas_limit = 4000000
priority_fee_gwei = 2.0
strategies = "two_hop_arb,jit"
min_profit_wei = 1000000000
output = "table"

# Disk-backed RPC config — GET returns raw URLs for local UI editing.
rpc_urls = ["https://user:secret@polygon-rpc.example.com/v1/SECRETKEY"]
rpc_rps = [10.0]

[chains.polygon]
chain_id = 137

[chains.arbitrum]
chain_id = 42161

[chains.bsc]
chain_id = 56

[explorer]
confirmations = 6
"#
        ),
    )
    .unwrap();
    path
}

/// A config file with a `${ENV_VAR}` placeholder in an RPC secret, for
/// testing that `PUT /api/config` round-trips placeholders verbatim.
pub fn write_env_template_config(dir: &Path) -> PathBuf {
    let path = dir.join("mev-scout.toml");
    std::fs::write(
        &path,
        r#"
chain = "polygon"
rpc_urls = ["https://user:${POLYGON_RPC_KEY}@polygon-rpc.example.com/v1"]

[chains.polygon]
chain_id = 137

[chains.arbitrum]
chain_id = 42161

[chains.bsc]
chain_id = 56

[explorer]
confirmations = 6
"#,
    )
    .unwrap();
    path
}
