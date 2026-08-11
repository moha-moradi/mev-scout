use alloy::primitives::Address;
use anyhow::Context;
use crate::dune::consts::DUNE_TIMEOUT_SECS;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing;

use super::types::*;

/// Dune Analytics API client.
///
/// Supports two execution modes:
/// 1. **Query by ID** — execute a pre-saved Dune query by its numeric ID.
/// 2. **Raw SQL** — Dune deprecated the raw-SQL execute endpoint, so raw SQL
///    is now executed by first creating a private query (`POST /v1/query`)
///    and then executing it by ID. Created queries are named after a stable
///    hash of the SQL and reused across runs, so the library stays clean.
///
/// # Note
/// Creating queries requires a Dune API key with `Read/Write` scope and an
/// Analyst plan (or higher). Free-tier keys can no longer run arbitrary SQL.
///
/// # Rate Limits
/// - Free tier: 1 query result / 5 seconds, 1,000 executions / hour
/// - Analyst tier: higher limits
///
/// # Example
/// ```ignore
/// let client = DuneClient::new("my-api-key");
/// let result = client.execute_query_by_id(12345, &[]).await?;
/// ```
pub struct DuneClient {
    api_key: String,
    http: reqwest::Client,
    base_url: String,
    /// SQL-hash → created query ID, so each SQL is created once per process.
    query_id_cache: Mutex<HashMap<u64, u64>>,
}

impl DuneClient {
    const DUNE_API_BASE: &'static str = "https://api.dune.com/api/v1";

    /// Prefix for queries auto-created from raw SQL.
    const QUERY_NAME_PREFIX: &'static str = "mev-scout-auto-";

    /// Create a new Dune API client.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .user_agent("mev-scout/0.1")
                .timeout(Duration::from_secs(DUNE_TIMEOUT_SECS))
                .build()
                .expect("reqwest Client::new"),
            base_url: Self::DUNE_API_BASE.to_string(),
            query_id_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Override the base URL (useful for testing or proxies).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    // ── Query by ID ──────────────────────────────────────────────────────

    /// Execute a pre-saved Dune query by its numeric ID.
    ///
    /// `params` is a flat map of query parameter key-value pairs (Dune's
    /// `{{param}}` syntax in the saved SQL).
    ///
    /// Polls until execution completes (with 1s backoff, up to 180s).
    pub async fn execute_query_by_id(
        &self,
        query_id: u64,
        params: &[(&str, &str)],
    ) -> anyhow::Result<DuneExecutionResult> {
        let url = format!("{}/query/{}/execute", self.base_url, query_id);

        let mut body = serde_json::Map::new();
        for (k, v) in params {
            body.insert(
                (*k).to_string(),
                Value::String((*v).to_string()),
            );
        }

        let resp: DuneExecutionResponse = self
            .http
            .post(&url)
            .header("x-dune-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to execute Dune query")?
            .error_for_status()
            .context("Dune query execution rejected")?
            .json()
            .await?;

        self.poll_execution(&resp.execution_id).await
    }

    /// Execute raw SQL directly on Dune.
    ///
    /// Dune deprecated the raw-SQL execute endpoint, so the SQL is first
    /// saved as a private query (named from a stable hash of the SQL) and
    /// then executed by ID. The created query is reused on later runs.
    pub async fn execute_raw_sql(
        &self,
        sql: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        self.execute_raw_sql_with_performance(sql, "medium").await
    }

    /// Execute raw SQL with explicit performance tier ("small", "medium", "large").
    ///
    /// The performance tier is accepted for API compatibility; the created
    /// query runs at its normal tier.
    pub async fn execute_raw_sql_with_performance(
        &self,
        sql: &str,
        _performance: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        let query_id = self.get_or_create_query_id(sql).await?;
        self.execute_query_by_id(query_id, &[]).await
    }

    // ── Raw-SQL via saved queries ───────────────────────────────────────

    /// Return the query ID for `sql`, creating a saved query if needed.
    ///
    /// The query is named from a stable hash of the SQL so that repeated
    /// runs across processes reuse the same saved query instead of
    /// accumulating duplicates in the Dune library.
    ///
    /// Queries are created public (`is_private: false`) because many Dune
    /// plans cap or forbid private queries, while public ones are unlimited.
    async fn get_or_create_query_id(&self, sql: &str) -> anyhow::Result<u64> {
        let key = Self::stable_hash(sql);
        let mut cache = self.query_id_cache.lock().await;

        if let Some(id) = cache.get(&key) {
            return Ok(*id);
        }

        let name = format!("{}{:016x}", Self::QUERY_NAME_PREFIX, key);
        let id = match self.find_query_id_by_name(&name).await? {
            Some(id) => {
                tracing::debug!("dune: reusing saved auto-query {} ({})", id, name);
                id
            }
            None => {
                tracing::info!(
                    "dune: creating private query for SQL hash {:016x}",
                    key
                );
                self.create_query(&name, sql).await?
            }
        };

        cache.insert(key, id);
        Ok(id)
    }

    /// Search the account's saved queries for one with the given name.
    async fn find_query_id_by_name(&self, name: &str) -> anyhow::Result<Option<u64>> {
        let limit = 100u64;
        let mut offset = 0u64;

        for _ in 0..10 {
            let url = format!(
                "{}/queries?limit={}&offset={}",
                self.base_url, limit, offset
            );
            let resp = self
                .http
                .get(&url)
                .header("x-dune-api-key", &self.api_key)
                .send()
                .await
                .context("failed to list Dune queries")?;

            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .context("failed to read Dune list-queries response")?;
            if !status.is_success() {
                anyhow::bail!(
                    "dune list queries rejected (HTTP {}): {}",
                    status,
                    body_text
                );
            }

            let list: DuneQueryList = serde_json::from_str(&body_text)
                .context("failed to parse Dune list-queries response")?;

            for q in &list.queries {
                if q.name == name {
                    return Ok(Some(q.id));
                }
            }

            let fetched = list.queries.len() as u64;
            if fetched < limit {
                break;
            }
            offset += fetched;
        }

        Ok(None)
    }

    /// Create a saved query from raw SQL and return its ID.
    ///
    /// Queries are created public so they don't count against Dune's
    /// private-query limits.
    async fn create_query(&self, name: &str, sql: &str) -> anyhow::Result<u64> {
        let url = format!("{}/query", self.base_url);
        let body = serde_json::json!({
            "name": name,
            "query_sql": sql,
            "is_private": false,
        });

        let resp = self
            .http
            .post(&url)
            .header("x-dune-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create Dune query")?;

        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .context("failed to read Dune create-query response")?;
        if !status.is_success() {
            anyhow::bail!(
                "dune query creation rejected (HTTP {}): {}\n\
                 Note: creating queries via the Dune API requires an Analyst \
                 plan or higher and a Read/Write API key — Dune has deprecated \
                 raw-SQL execution.",
                status,
                body_text
            );
        }

        let created: DuneCreateQueryResponse = serde_json::from_str(&body_text)
            .context("failed to parse Dune create-query response")?;
        Ok(created.query_id)
    }

    /// Stable (non-randomized) 64-bit hash of a string, used to derive a
    /// deterministic query name so identical SQL reuses the same saved query.
    fn stable_hash(s: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
        for b in s.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Poll execution status until completed or failed.
    async fn poll_execution(
        &self,
        execution_id: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        let status_url = format!(
            "{}/execution/{}/status",
            self.base_url, execution_id
        );
        let results_url = format!(
            "{}/execution/{}/results",
            self.base_url, execution_id
        );

        let max_polls = 580; // 580 seconds max
        for _ in 0..max_polls {
            let response = self
                .http
                .get(&status_url)
                .header("x-dune-api-key", &self.api_key)
                .send()
                .await
                .context("Failed to poll Dune execution status")?;

            if response.status().as_u16() == 429 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(5);
                tracing::warn!("  Rate limited, waiting {}s...", retry_after);
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                continue;
            }

            let status: DuneExecutionStatus = response
                .error_for_status()?
                .json()
                .await?;

            match status.state.as_str() {
                "QUERY_STATE_COMPLETED" => {
                    let results: DuneExecutionResult = self
                        .http
                        .get(&results_url)
                        .header("x-dune-api-key", &self.api_key)
                        .send()
                        .await
                        .context("Failed to fetch Dune query results")?
                        .error_for_status()?
                        .json()
                        .await?;
                    return Ok(results);
                }
                "QUERY_STATE_COMPLETED_PARTIAL" => {
                    let results: DuneExecutionResult = self
                        .http
                        .get(&results_url)
                        .header("x-dune-api-key", &self.api_key)
                        .send()
                        .await
                        .context("Failed to fetch Dune query results")?
                        .error_for_status()?
                        .json()
                        .await?;
                    return Ok(results);
                }
                s if s == "QUERY_STATE_FAILED" || s == "QUERY_STATE_CANCELED" || s == "QUERY_STATE_EXPIRED" => {
                    let msg = status.error.map(|e| e.message).unwrap_or_default();
                    return Err(anyhow::anyhow!("dune query {}: {}", s, msg));
                }
                _ => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }

        Err(anyhow::anyhow!(
            "dune query timed out after {} seconds",
            max_polls
        ))
    }

    // ── Convenience helpers ──────────────────────────────────────────────

    pub fn col_as_string(row: &DuneRow, col_name: &str) -> Option<String> {
        row.get(col_name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn col_as_u64(row: &DuneRow, col_name: &str) -> Option<u64> {
        row.get(col_name)
            .and_then(|v| {
                if let Some(n) = v.as_u64() {
                    return Some(n);
                }
                if let Some(s) = v.as_str() {
                    return s.parse::<u64>().ok();
                }
                if let Some(n) = v.as_f64() {
                    return Some(n as u64);
                }
                None
            })
    }

    pub fn col_as_address(row: &DuneRow, col_name: &str) -> Option<Address> {
        Self::col_as_string(row, col_name)
            .and_then(|s| s.parse::<Address>().ok())
    }

    pub fn col_as_f64(row: &DuneRow, col_name: &str) -> Option<f64> {
        row.get(col_name)
            .and_then(|v| {
                if let Some(n) = v.as_f64() {
                    return Some(n);
                }
                if let Some(s) = v.as_str() {
                    return s.parse::<f64>().ok();
                }
                None
            })
    }
}

