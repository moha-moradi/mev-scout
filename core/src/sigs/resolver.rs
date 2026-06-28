use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};

use alloy::primitives::B256;

const CACHE_CAPACITY: usize = 10_000;

/// Resolves 4-byte function selectors and 32-byte event topic hashes to
/// human-readable signatures using a pre-built SQLite database.
pub struct SignatureResolver {
    /// Read-only connection to the signature database.
    conn: Mutex<rusqlite::Connection>,
    /// Cached method lookups: 4-byte selector → signature.
    method_cache: RwLock<HashMap<[u8; 4], Option<String>>>,
    /// Cached event lookups: 32-byte topic hash → signature.
    event_cache: RwLock<HashMap<B256, Option<String>>>,
}

impl SignatureResolver {
    /// Open a signature database at the given path.
    ///
    /// The database is opened read-only. An error is returned if the file
    /// does not exist or is not a valid SQLite database.
    pub fn new(db_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open_with_flags(
            db_path.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        // Quick sanity check: does the methods table exist?
        let has_methods: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='methods'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_methods {
            anyhow::bail!(
                "Signature database at {} is missing the 'methods' table",
                db_path.as_ref().display()
            );
        }

        Ok(SignatureResolver {
            conn: Mutex::new(conn),
            method_cache: RwLock::new(HashMap::new()),
            event_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Resolve a 4-byte function selector to a human-readable method signature.
    ///
    /// Checks the in-memory cache first, then queries the sig DB.
    /// Returns `Ok(None)` if the selector is unknown.
    pub fn resolve_method(&self, selector: &[u8; 4]) -> anyhow::Result<Option<String>> {
        // Check cache
        if let Some(result) = self.method_cache.read().unwrap().get(selector) {
            return Ok(result.clone());
        }

        // Query DB
        let conn = self.conn.lock().unwrap();
        let result: Option<String> = conn
            .query_row(
                "SELECT signature FROM methods WHERE selector = ?1",
                rusqlite::params![selector.to_vec()],
                |row| row.get(0),
            )
            .ok();

        // Cache result (even None — negative caching)
        {
            let mut cache = self.method_cache.write().unwrap();
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
            cache.insert(*selector, result.clone());
        }

        Ok(result)
    }

    /// Resolve a 32-byte event topic hash to a human-readable event signature.
    ///
    /// Checks the in-memory cache first, then queries the sig DB.
    /// Returns `Ok(None)` if the topic is unknown.
    pub fn resolve_event(&self, topic: &B256) -> anyhow::Result<Option<String>> {
        // Check cache
        if let Some(result) = self.event_cache.read().unwrap().get(topic) {
            return Ok(result.clone());
        }

        // Query DB
        let conn = self.conn.lock().unwrap();
        let result: Option<String> = conn
            .query_row(
                "SELECT signature FROM events WHERE topic = ?1",
                rusqlite::params![topic.as_slice().to_vec()],
                |row| row.get(0),
            )
            .ok();

        // Cache result
        {
            let mut cache = self.event_cache.write().unwrap();
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
            cache.insert(*topic, result.clone());
        }

        Ok(result)
    }
}
