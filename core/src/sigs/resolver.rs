use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use alloy::primitives::B256;
use reqwest::Client as HttpClient;
use serde::Deserialize;

const CACHE_CAPACITY: usize = 10_000;
const FOURBYTE_API: &str = "https://www.4byte.directory/api/v1/signatures/";
const API_RATE_LIMIT: Duration = Duration::from_millis(200);

/// Response from 4byte.directory API for a signature lookup.
#[derive(Deserialize)]
struct FourByteResponse {
    count: Option<u64>,
    results: Vec<FourByteResult>,
}

#[derive(Deserialize)]
struct FourByteResult {
    text_signature: String,
}

/// Resolves 4-byte function selectors and 32-byte event topic hashes to
/// human-readable signatures using a pre-built SQLite database, with an
/// optional live fallback to 4byte.directory API.
pub struct SignatureResolver {
    /// Read-only connection to the signature database.
    conn: Mutex<rusqlite::Connection>,
    /// Optional HTTP client for 4byte.directory API lookups.
    http_client: Option<HttpClient>,
    /// Timestamp of the last API call for rate limiting.
    last_api_call: Mutex<Instant>,
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
    /// An HTTP client is created for potential 4byte.directory API fallback.
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

        let http_client = HttpClient::builder()
            .timeout(Duration::from_secs(5))
            .user_agent("mev-scout/0.1.0")
            .build()
            .ok();

        Ok(SignatureResolver {
            conn: Mutex::new(conn),
            http_client,
            last_api_call: Mutex::new(Instant::now()),
            method_cache: RwLock::new(HashMap::new()),
            event_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Resolve a 4-byte function selector to a human-readable method signature.
    ///
    /// Checks the in-memory cache first, then queries the sig DB,
    /// then falls back to 4byte.directory API.
    /// Returns `Ok(None)` if the selector is unknown.
    pub fn resolve_method(&self, selector: &[u8; 4]) -> anyhow::Result<Option<String>> {
        // Check cache
        if let Some(result) = self.method_cache.read().unwrap().get(selector) {
            return Ok(result.clone());
        }

        // Query DB
        let result: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT signature FROM methods WHERE selector = ?1",
                rusqlite::params![selector.to_vec()],
                |row| row.get(0),
            )
            .ok()
        };

        if result.is_some() {
            // Cache positive result and return
            let mut cache = self.method_cache.write().unwrap();
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
            cache.insert(*selector, result.clone());
            return Ok(result);
        }

        // Fallback: query 4byte.directory API
        let api_result = self.resolve_from_api(selector);

        // Cache whatever we got (even None to avoid repeated API calls)
        {
            let mut cache = self.method_cache.write().unwrap();
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
            cache.insert(*selector, api_result.clone());
        }

        Ok(api_result)
    }

    /// Query 4byte.directory API for a given selector.
    fn resolve_from_api(&self, selector: &[u8; 4]) -> Option<String> {
        let client = self.http_client.as_ref()?;
        let hex_sel = hex::encode(selector);
        let url = format!("{}?hex_signature=0x{}", FOURBYTE_API, hex_sel);

        // Rate limit
        {
            let mut last = self.last_api_call.lock().unwrap();
            let elapsed = last.elapsed();
            if elapsed < API_RATE_LIMIT {
                std::thread::sleep(API_RATE_LIMIT - elapsed);
            }
            *last = Instant::now();
        }

        // Use block_in_place since we're already inside a tokio runtime.
        let resp = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(client.get(&url).send())
        });
        match resp {
            Ok(resp) if resp.status().is_success() => {
                let data = tokio::task::block_in_place(|| {
                    let handle = tokio::runtime::Handle::current();
                    handle.block_on(resp.json::<FourByteResponse>())
                });
                match data {
                    Ok(data) => {
                        let count = data.count.unwrap_or(0);
                        if count > 0 {
                            data.results.into_iter().next().map(|r| r.text_signature)
                        } else {
                            None
                        }
                    }
                    Err(e) => {
                        tracing::trace!("4byte.directory JSON parse error: {e}");
                        None
                    }
                }
            }
            Ok(resp) => {
                tracing::trace!("4byte.directory HTTP {} for 0x{hex_sel}", resp.status());
                None
            }
            Err(e) => {
                tracing::trace!("4byte.directory request failed for 0x{hex_sel}: {e}");
                None
            }
        }
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
