//! Shared application state: config, DB connections, job manager.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mev_scout_core::config::Config;
use mev_scout_core::types::ChainName;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};

use crate::jobs::JobManager;

/// Per-chain DB status for `/api/chains` and `/api/health`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbStatus {
    Missing,
    Ok,
    Corrupt,
}

impl DbStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DbStatus::Missing => "missing",
            DbStatus::Ok => "ok",
            DbStatus::Corrupt => "corrupt",
        }
    }
}

/// Check a candidate DB path: missing file → `Missing`; openable + has
/// mev-scout tables → `Ok`; open fails / query fails → `Corrupt`.
pub fn check_db_status(path: &Path, probe_table: &str) -> DbStatus {
    if !path.exists() {
        return DbStatus::Missing;
    }
    match Connection::open(path) {
        Ok(conn) => match conn.query_row(
            &format!("SELECT 1 FROM sqlite_master WHERE type='table' AND name='{probe_table}'"),
            [],
            |r| r.get::<_, i64>(0),
        ) {
            Ok(_) => DbStatus::Ok,
            // File exists and opens, but lacks the probe table: treat as
            // corrupt/foreign rather than ok.
            Err(_) => DbStatus::Corrupt,
        },
        Err(_) => DbStatus::Corrupt,
    }
}

/// Application state shared by all handlers.
pub struct AppState {
    /// Active config (env-expanded, in memory). Guarded because a chain
    /// switch (`PUT /api/config`) rewrites the config and re-resolves DBs.
    pub config: RwLock<Config>,
    /// Path of the config file on disk (edits round-trip the raw TOML,
    /// never this expanded copy).
    pub config_path: PathBuf,
    /// Active explorer DB path (derived from active chain; swapped on edit).
    pub explorer_db_path: RwLock<PathBuf>,
    /// Active scanner-cache DB path (derived from active chain; swapped on edit).
    pub cache_db_path: RwLock<PathBuf>,
    /// Read-only explorer SQLite connection (single, mutex-guarded).
    pub explorer_conn: Mutex<Connection>,
    /// Read-only scanner-cache SQLite connection (single, mutex-guarded).
    pub cache_conn: Mutex<Connection>,
    /// Job manager (single running job at a time).
    pub job_manager: Arc<Mutex<JobManager>>,
    /// Server start time (uptime reporting).
    pub started_at: std::time::Instant,
    /// API version reported by /api/health.
    pub version: &'static str,
}

/// The derived (default) DB path for a chain when the config leaves
/// `output.db_path` / `explorer.db_path` empty: `./cache/{chain}-mev-scout.sqlite`
/// and `./cache/explorer-{chain}.sqlite`.
pub fn derived_cache_db_path(chain: ChainName) -> PathBuf {
    PathBuf::from(format!("./cache/{}-mev-scout.sqlite", chain))
}

pub fn derived_explorer_db_path(chain: ChainName) -> PathBuf {
    PathBuf::from(format!("./cache/explorer-{}.sqlite", chain))
}

impl AppState {
    pub async fn active_chain(&self) -> ChainName {
        self.config.read().await.chain
    }

    /// Resolve both effective DB paths from the current config + active chain,
    /// then (re)open both read-only connections. Called at startup and after
    /// a config edit that changes `chain` or `output.db_path`/`explorer.db_path`.
    pub async fn resolve_connections(&self, paths_changed: bool) -> anyhow::Result<()> {
        if !paths_changed {
            return Ok(());
        }
        let cfg = self.config.read().await;
        let chain = cfg.chain;
        let cache_path = PathBuf::from(cfg.effective_db_path(&chain));
        let explorer_path = PathBuf::from(cfg.effective_explorer_db_path(&chain));
        drop(cfg);

        let cache_conn = open_readonly_if_exists(&cache_path)?;
        let explorer_conn = open_read_only_or_empty(&explorer_path)?;

        *self.cache_db_path.write().await = cache_path;
        *self.explorer_db_path.write().await = explorer_path;
        *self.cache_conn.lock().await = cache_conn;
        *self.explorer_conn.lock().await = explorer_conn;
        Ok(())
    }

    /// Reopen the explorer connection when the on-disk DB appeared after we
    /// started with an in-memory placeholder (common: first `explorer index`
    /// creates `cache/explorer-{chain}.sqlite` while the API still holds the
    /// empty scratch DB opened at boot).
    pub async fn ensure_explorer_conn(&self) -> anyhow::Result<()> {
        let path = self.explorer_db_path.read().await.clone();
        if !path.exists() {
            return Ok(());
        }
        let mut conn = self.explorer_conn.lock().await;
        let file: String = conn
            .query_row(
                "SELECT file FROM pragma_database_list WHERE seq = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        if file.is_empty() {
            *conn = open_read_only_or_empty(&path)?;
        }
        Ok(())
    }

    /// Same as [`Self::ensure_explorer_conn`] for the scanner-cache DB.
    pub async fn ensure_cache_conn(&self) -> anyhow::Result<()> {
        let path = self.cache_db_path.read().await.clone();
        if !path.exists() {
            return Ok(());
        }
        let mut conn = self.cache_conn.lock().await;
        let file: String = conn
            .query_row(
                "SELECT file FROM pragma_database_list WHERE seq = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        if file.is_empty() {
            *conn = open_readonly_if_exists(&path)?;
        }
        Ok(())
    }
}

/// Open a read-only connection when the file exists; otherwise return a
/// connection to an in-memory scratch DB seeded with the mev-scout schema so
/// read endpoints degrade to empty results instead of 500s.
pub fn open_read_only_or_empty(path: &Path) -> anyhow::Result<Connection> {
    if path.exists() {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(conn)
    } else {
        // In-memory placeholder with the explorer schema so queries on a
        // missing DB return empty sets rather than "no such table" errors.
        let conn = Connection::open_in_memory()?;
        let schema = include_str!("explorer_schema.sql");
        conn.execute_batch(schema)?;
        Ok(conn)
    }
}

pub fn open_readonly_if_exists(path: &Path) -> anyhow::Result<Connection> {
    if path.exists() {
        Ok(Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?)
    } else {
        // Scanner cache schema is created lazily by the CLI; an API-side
        // read against a missing DB is served by an empty in-memory DB
        // seeded with the cache schema.
        let conn = Connection::open_in_memory()?;
        let schema = include_str!("cache_schema.sql");
        conn.execute_batch(schema)?;
        Ok(conn)
    }
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// Open the cache DB through `SqliteStore` only when the file exists.
    /// `SqliteStore::open` creates a fresh DB when missing, which would
    /// fabricate empty stores on read-only endpoints — degrade to an error
    /// the routes translate to empty results instead.
    pub async fn open_cache_store_if_exists(
        &self,
    ) -> anyhow::Result<mev_scout_core::cache::SqliteStore> {
        let path = self.cache_db_path.read().await.clone();
        if !path.exists() {
            anyhow::bail!("cache DB not found at {}", path.display());
        }
        mev_scout_core::cache::SqliteStore::open(&path)
    }

    /// Open the explorer DB through `ExplorerStore` only when the file exists.
    /// Same missing-file guard as [`Self::open_cache_store_if_exists`].
    pub async fn open_explorer_store_if_exists(
        &self,
    ) -> anyhow::Result<mev_scout_core::explorer::store::ExplorerStore> {
        let path = self.explorer_db_path.read().await.clone();
        if !path.exists() {
            anyhow::bail!("explorer DB not found at {}", path.display());
        }
        mev_scout_core::explorer::store::ExplorerStore::open(&path)
    }
}
