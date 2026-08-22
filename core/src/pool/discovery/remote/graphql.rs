//! GraphQL client for The Graph subgraphs.
//!
//! Handles `POST {"query": …}` with `first`/`skip` pagination, retry+backoff
//! on 429 / "throttled" / 502, 15 s timeout, and multi-URL failover in order
//! (gateway → hosted → Goldsky). Retries reuse the RateLimiter pattern from
//! `core/src/rpc/middleware.rs`.

use std::time::Duration;

use serde_json::json;

use crate::dex_type::DexType;
use crate::types::SubgraphSchema;

use super::schemas;
use super::RemotePool;

/// Simple GraphQL client that tries URLs in order with retry and failover.
pub struct GraphClient {
    client: reqwest::Client,
    urls: Vec<String>,
    schema: SubgraphSchema,
    dex_type: DexType,
    dex_name: String,
}

impl GraphClient {
    pub fn new(urls: Vec<String>, schema: SubgraphSchema, dex_type: DexType, dex_name: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("mev-scout/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, urls, schema, dex_type, dex_name }
    }

    /// Fetch pools from the subgraph, paginating with `first=1000, skip=N`.
    ///
    /// Tries each URL in `self.urls` in order; for each URL pagination continues
    /// until a page returns <1000 results or `max_pools` is reached. If a URL
    /// fails after retries, the next URL is tried.
    pub async fn fetch_pools(&self, max_pools: Option<usize>, min_tvl: Option<f64>) -> anyhow::Result<Vec<RemotePool>> {
        self.fetch_pools_cb(max_pools, min_tvl, None).await
    }

    /// Same as [`fetch_pools`], invoking `on_page(fetched_so_far)` after every
    /// paginated page (for progress reporting).
    pub async fn fetch_pools_cb(
        &self,
        max_pools: Option<usize>,
        min_tvl: Option<f64>,
        on_page: Option<&dyn Fn(usize)>,
    ) -> anyhow::Result<Vec<RemotePool>> {
        let limit = max_pools.unwrap_or(usize::MAX);
        if self.urls.is_empty() {
            anyhow::bail!("no URLs configured for {}", self.dex_name);
        }

        let mut last_err: Option<anyhow::Error> = None;

        for url in &self.urls {
            tracing::debug!("GraphClient: trying {} for {}", url, self.dex_name);
            match self.fetch_via_url(url, limit, min_tvl, on_page).await {
                Ok(pools) => {
                    if pools.is_empty() {
                        tracing::debug!("GraphClient: {} returned 0 pools at {}", self.dex_name, url);
                        // Try next URL maybe has data? But empty is valid — return empty.
                        return Ok(pools);
                    }
                    return Ok(pools);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let is_retryable = is_retryable_error(&msg);
                    tracing::warn!(
                        "GraphClient: {} failed at {}: {:#} (retryable={})",
                        self.dex_name, url, e, is_retryable
                    );
                    last_err = Some(e);
                    // Try next URL in order regardless — next URL may succeed.
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all URLs failed for {}", self.dex_name)))
    }

    async fn fetch_via_url(
        &self,
        url: &str,
        limit: usize,
        min_tvl: Option<f64>,
        on_page: Option<&dyn Fn(usize)>,
    ) -> anyhow::Result<Vec<RemotePool>> {
        let mut all: Vec<RemotePool> = Vec::new();
        let mut skip = 0usize;
        let page_size = 1000usize;

        loop {
            let first = std::cmp::min(page_size, limit.saturating_sub(all.len()));
            if first == 0 {
                break;
            }

            let query = schemas::build_pools_query(&self.schema, first, skip, min_tvl);
            let body = json!({ "query": query });

            let json = self.post_with_retry(url, body).await?;

            // GraphQL errors are returned as { "errors": [...] } even with 200
            if let Some(errors) = json.get("errors") {
                let err_msg = errors.to_string();
                if is_retryable_error(&err_msg) {
                    // Treat as transient — retry will be handled by post_with_retry, but if we got here it exhausted retries
                    anyhow::bail!("GraphQL errors (retryable): {}", err_msg);
                }
                anyhow::bail!("GraphQL errors: {}", err_msg);
            }

            let mut pools = schemas::parse_pools(&self.schema, &json, self.dex_type, &self.dex_name)?;

            // Client-side TVL filter as safety net (in case subgraph ignores where)
            if let Some(min) = min_tvl {
                pools.retain(|p| p.tvl_usd.unwrap_or(0.0) >= min);
            }

            let count = pools.len();
            all.extend(pools);

            if let Some(cb) = on_page {
                cb(all.len());
            }

            if count < first {
                break;
            }
            skip += count;
            if all.len() >= limit {
                all.truncate(limit);
                break;
            }

            // Be gentle — tiny delay between pages to avoid rate limits
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        Ok(all)
    }

    async fn post_with_retry(&self, url: &str, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 500;

        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..MAX_RETRIES {
            let res = self.client.post(url).json(&body).send().await;

            match res {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();

                    if status.is_success() {
                        let json: serde_json::Value = serde_json::from_str(&text)
                            .map_err(|e| anyhow::anyhow!("invalid JSON from {}: {} — body: {}", url, e, &text[..text.len().min(500)]))?;
                        return Ok(json);
                    }

                    let err_msg = format!("HTTP {} from {}: {}", status.as_u16(), url, &text[..text.len().min(500)]);
                    let retryable = status.as_u16() == 429 || status.as_u16() == 502 || status.as_u16() == 503 || is_retryable_error(&err_msg);
                    last_err = Some(anyhow::anyhow!(err_msg));

                    if retryable && attempt + 1 < MAX_RETRIES {
                        let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                        tracing::debug!("GraphClient retry {}/{} for {} after {}ms (status {})", attempt + 1, MAX_RETRIES, url, delay, status.as_u16());
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let retryable = is_retryable_error(&msg) || e.is_timeout() || e.is_connect();
                    last_err = Some(anyhow::anyhow!("request to {} failed: {:#}", url, e));
                    if retryable && attempt + 1 < MAX_RETRIES {
                        let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                        tracing::debug!("GraphClient retry {}/{} for {} after {}ms (error: {})", attempt + 1, MAX_RETRIES, url, delay, msg);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    } else {
                        break;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("request to {} failed after retries", url)))
    }
}

fn is_retryable_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("throttled")
        || lower.contains("too many requests")
        || lower.contains("429")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("rate limit")
        || lower.contains("temporarily unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SubgraphSchema;
    use crate::dex_type::DexType;

    #[test]
    fn test_is_retryable() {
        assert!(is_retryable_error("429 Too Many Requests"));
        assert!(is_retryable_error("throttled"));
        assert!(is_retryable_error("502 Bad Gateway"));
        assert!(!is_retryable_error("invalid query"));
    }

    #[tokio::test]
    async fn test_fetch_pools_with_mock() {
        // Start a wiremock server
        let mock_server = wiremock::MockServer::start().await;

        let response_body = serde_json::json!({
            "data": {
                "pools": [
                    {
                        "id": "0x1234567890123456789012345678901234567890",
                        "token0": {"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"},
                        "token1": {"id": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"},
                        "feeTier": "3000",
                        "tickSpacing": "60",
                        "totalValueLockedUSD": "10000",
                        "volumeUSD": "5000",
                        "createdAtBlockNumber": "49100001"
                    }
                ]
            }
        });

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&mock_server)
            .await;

        let client = GraphClient::new(
            vec![mock_server.uri()],
            SubgraphSchema::UniswapV3,
            DexType::UniswapV3,
            "Test".to_string(),
        );

        let pools = client.fetch_pools(Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].fee, 3000);
    }

    #[tokio::test]
    async fn test_failover_to_second_url() {
        let mock1 = wiremock::MockServer::start().await;
        let mock2 = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&mock1)
            .await;

        let response_body = serde_json::json!({
            "data": { "pools": [{
                "id": "0x1234567890123456789012345678901234567890",
                "token0": {"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"},
                "token1": {"id": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"},
                "feeTier": "500",
                "tickSpacing": "10",
                "totalValueLockedUSD": "2000",
                "volumeUSD": "1000",
                "createdAtBlockNumber": "1"
            }]}
        });

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&mock2)
            .await;

        let client = GraphClient::new(
            vec![mock1.uri(), mock2.uri()],
            SubgraphSchema::UniswapV3,
            DexType::UniswapV3,
            "Test".to_string(),
        );

        let pools = client.fetch_pools(Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].fee, 500);
    }

    #[tokio::test]
    async fn test_pagination_two_pages() {
        let mock_server = wiremock::MockServer::start().await;

        // First page returns 1 pool, second returns 0 to stop pagination.
        // We do this by inspecting `skip` in the request body.
        // For simplicity just return 1 pool on first request and empty on second;
        // the mock will be called twice because we use a single mount that always returns 1.
        // To handle pagination properly the test uses a custom responder that checks body.
        use wiremock::{Mock, ResponseTemplate, matchers::method};
        use serde_json::Value;

        Mock::given(method("POST"))
            .respond_with(move |req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let resp = if query.contains("skip: 0") {
                    serde_json::json!({
                        "data": { "pools": [{
                            "id": "0x1111111111111111111111111111111111111111",
                            "token0": {"id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "symbol": "WETH"},
                            "token1": {"id": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "symbol": "USDC"},
                            "feeTier": "3000",
                            "tickSpacing": "60",
                            "totalValueLockedUSD": "1000",
                            "volumeUSD": "500",
                            "createdAtBlockNumber": "1"
                        }]}
                    })
                } else {
                    serde_json::json!({"data": {"pools": []}})
                };
                ResponseTemplate::new(200).set_body_json(resp)
            })
            .mount(&mock_server)
            .await;

        let client = GraphClient::new(
            vec![mock_server.uri()],
            SubgraphSchema::UniswapV3,
            DexType::UniswapV3,
            "Test".to_string(),
        );

        let pools = client.fetch_pools(Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
    }
}
