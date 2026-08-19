use alloy::primitives::Address;
use anyhow::Context;
use crate::dune::consts::DUNE_TIMEOUT_SECS;
use serde_json::Value;
use std::time::Duration;
use tracing;

use super::types::*;

/// Dune Analytics API client.
///
/// Supports two execution modes:
/// 1. **Query by ID** — execute a pre-saved Dune query by its numeric ID.
/// 2. **Raw SQL** — execute arbitrary SQL via `POST /v1/sql/execute` with
///    `"engine": "dune_sql"` (DuneSQL / Engine v2), which is required since
///    Dune deprecated the old query engine.
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
}

impl DuneClient {
    const DUNE_API_BASE: &'static str = "https://api.dune.com/api/v1";

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
        self.execute_query_by_id_with_performance(query_id, params, "").await
    }

    /// Execute a saved query with an explicit performance tier.
    ///
    /// Community/free plans typically require `"free"`; paid plans use
    /// `"small"`, `"medium"`, or `"large"`.
    pub async fn execute_query_by_id_with_performance(
        &self,
        query_id: u64,
        params: &[(&str, &str)],
        performance: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        let url = format!("{}/query/{}/execute", self.base_url, query_id);

        let mut body = serde_json::Map::new();
        for (k, v) in params {
            body.insert(
                (*k).to_string(),
                Value::String((*v).to_string()),
            );
        }
        if !performance.is_empty() {
            body.insert(
                "performance".to_string(),
                Value::String(performance.to_string()),
            );
        }

        let resp = self
            .http
            .post(&url)
            .header("x-dune-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to execute Dune query")?;

        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .context("Failed to read Dune execute response")?;

        if !status.is_success() {
            anyhow::bail!(
                "Dune query execution rejected (HTTP {}): {}",
                status,
                body_text
            );
        }

        let resp: DuneExecutionResponse = serde_json::from_str(&body_text)
            .context("Failed to parse Dune execute response")?;

        self.poll_execution(&resp.execution_id).await
    }

    /// Execute raw SQL on Dune via `POST /v1/sql/execute`.
    ///
    /// The `engine` parameter is set to `"dune_sql"` (DuneSQL / Engine v2)
    /// which is required since Dune deprecated the old query engine.
    pub async fn execute_raw_sql(
        &self,
        sql: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        self.execute_raw_sql_with_performance(sql, "small").await
    }

    /// Execute raw SQL with explicit performance tier
    /// (`"free"`, `"small"`, `"medium"`, or `"large"`).
    pub async fn execute_raw_sql_with_performance(
        &self,
        sql: &str,
        performance: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        let url = format!("{}/sql/execute", self.base_url);
        let body = if performance.is_empty() {
            serde_json::json!({
                "sql": sql,
            })
        } else {
            // NOTE: Dune deprecated the "engine" field (previously "dune_sql").
            // The new API accepts just "sql" + "performance".
            serde_json::json!({
                "sql": sql,
                "performance": performance,
            })
        };

        let resp = self
            .http
            .post(&url)
            .header("x-dune-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to execute Dune SQL")?;

        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .context("Failed to read Dune sql/execute response")?;

        if !status.is_success() {
            anyhow::bail!(
                "Dune SQL execution rejected (HTTP {}): {}",
                status,
                body_text
            );
        }

        let resp: DuneExecutionResponse = serde_json::from_str(&body_text)
            .context("Failed to parse Dune sql/execute response")?;

        self.poll_execution(&resp.execution_id).await
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

