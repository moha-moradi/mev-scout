use alloy::primitives::Address;
use anyhow::Context;
use serde_json::Value;
use std::time::Duration;

use super::types::*;

/// Dune Analytics API client.
///
/// Supports two execution modes:
/// 1. **Query by ID** — execute a pre-saved Dune query by its numeric ID.
/// 2. **Raw SQL** — execute arbitrary SQL directly.
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
                .timeout(Duration::from_secs(180))
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
    pub async fn execute_raw_sql(
        &self,
        sql: &str,
    ) -> anyhow::Result<DuneExecutionResult> {
        let url = format!("{}/sql/execute", self.base_url);

        let body = serde_json::json!({
            "sql": sql,
            "performance": "medium",
        });

        let response = self
            .http
            .post(&url)
            .header("x-dune-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to execute raw Dune SQL")?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Dune raw SQL execution rejected (HTTP {}): {}",
                status,
                body_text
            );
        }

        let resp: DuneExecutionResponse = response.json().await?;

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

        let max_polls = 120; // 120 seconds max
        for _ in 0..max_polls {
            let status: DuneExecutionStatus = self
                .http
                .get(&status_url)
                .header("x-dune-api-key", &self.api_key)
                .send()
                .await
                .context("Failed to poll Dune execution status")?
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
                    return Err(anyhow::anyhow!("Dune query {}: {}", s, msg));
                }
                _ => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }

        Err(anyhow::anyhow!(
            "Dune query timed out after {} seconds",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = DuneClient::new("test-key");
        assert!(client.api_key == "test-key");
    }
}
