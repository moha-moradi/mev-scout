//! Multi-provider RPC client with per-endpoint rate limiting, weighted selection,
//! and block-range sharding for load distribution across public/private RPC endpoints.

use std::sync::atomic::{AtomicUsize, Ordering};
use crate::rpc::consts::{DEAD_PROVIDER_COOLDOWN_SECS, HTTP_TIMEOUT_SECS};
use std::sync::Arc;
use std::time::Instant;

use alloy::consensus::Transaction;
use alloy::eips::BlockId;
use alloy::eips::BlockNumberOrTag;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::rpc::types::{Block, Filter, Log, Transaction as AlloyTx, TransactionReceipt};
use alloy::rpc::client::{BatchRequest, RpcClient as AlloyRpcClient, Waiter};
use futures;
use serde_json::Value;
use url::Url;
use crate::data::types::{AccessListItem, BlockData, LogData, ReceiptData, TxData};

use super::middleware::{ProviderState, RateLimiter};

/// Block reference for pool-state queries: either an explicit block number
/// (requires archive/recent-state-capable providers) or the `latest` tag
/// (served by any full node).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRef {
    Number(u64),
    Latest,
}

/// Whether an RPC error message describes a transient transport-level failure
/// (fresh-connection handshake resets, timeouts, connection closed/refused,
/// connection send failures) or an upstream rate-limit signal (HTTP 429).
/// These are safe to retry rather than definitive JSON-RPC responses
/// (e.g. reverts, missing state).
fn is_transport_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("connection")
        || m.contains("error sending request")
        || m.contains("send request")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("reset")
        || m.contains("refused")
        || m.contains("closed")
        || is_rate_limit_error(msg)
}

/// Whether an RPC error message signals an upstream rate limit (HTTP 429).
/// Rate-limited requests should trigger a rate reduction + retry rather than
/// the full failure/cooldown penalty, since the endpoint is still healthy.
fn is_rate_limit_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("429")
        || m.contains("too many requests")
        || m.contains("rate limit")
        || m.contains("throttl")
        || m.contains("out of cu")
        || m.contains("402")
}

/// Whether an RPC error message means the requested historical state is
/// deterministically unavailable on this provider (state pruned / not an
/// archive node, e.g. `historical state 0x.. is not available` from
/// GetBlock, `missing trie node` from Geth).
///
/// The provider itself is healthy — this is a capability limitation, not a
/// failure — so it must NOT trigger the failure/cooldown penalty. Treating it
/// as a failure poisons the whole replay: a few state-unavailable hits put the
/// only non-archive providers into exponential-backoff cooldown and every
/// subsequent tx fails with "all RPC providers exhausted or in cooldown".
fn is_state_unavailable_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("historical state")
        || m.contains("state is not available")
        || m.contains("state not available")
        || m.contains("no state available")
        || m.contains("missing trie node")
        || m.contains("missing trie")
        || m.contains("trie node")
        || m.contains("cannot fetch a block number in the future")
        || m.contains("header not found")
        || m.contains("no header for hash")
}

/// Retry an RPC future a few times when it fails with a transient transport
/// error. Definitive JSON-RPC errors are returned immediately (not retried).
async fn retry_transport<F, Fut, T, E>(mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    const MAX_ATTEMPTS: usize = 3;
    let mut attempts = 0;
    loop {
        attempts += 1;
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                let msg = e.to_string();
                if attempts < MAX_ATTEMPTS && is_transport_error(&msg) {
                    tokio::time::sleep(
                        tokio::time::Duration::from_millis(400 * attempts as u64),
                    )
                    .await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Multi-provider RPC client with per-endpoint rate limiting, weighted selection,
/// and health tracking.
///
/// Each provider has its own rate limiter. When an RPC call fails, the provider
/// enters a cooldown with exponential backoff. Available providers are selected
/// by weighted random selection (weight = RPS).
#[derive(Debug, Clone)]
pub struct RpcClient {
    providers: Arc<tokio::sync::Mutex<Vec<ProviderState>>>,
    chain_id: u64,
    current: Arc<AtomicUsize>,
    /// Round-robin offset so consecutive calls lead with a different provider.
    /// Without this, equal-weight providers stable-sort onto the same winner
    /// and all traffic pins to a single rate limiter (e.g. one 10 RPS endpoint
    /// while four identical siblings sit idle).
    dispatch_counter: Arc<AtomicUsize>,
}

impl RpcClient {
    /// Create a new RPC client from a single URL and expected chain ID.
    ///
    /// Backward-compatible convenience wrapper around `from_urls`.
    pub fn new(rpc_url: &str, chain_id: u64) -> anyhow::Result<Self> {
        Self::from_urls(&[rpc_url], chain_id)
    }

    /// Create a new RPC client from one or more URLs.
    ///
    /// Each URL gets its own `ProviderState` with no rate limiter (use
    /// `with_provider_rps` to set per-provider limits).
    pub fn from_urls(urls: &[&str], chain_id: u64) -> anyhow::Result<Self> {
        if urls.is_empty() {
            anyhow::bail!("at least one RPC URL is required");
        }
        let http_client = Self::build_http_client()?;
        let providers: Vec<ProviderState> = urls
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let u: Url = url.parse().map_err(|e| anyhow::anyhow!("invalid RPC URL '{url}': {e}"))?;
                let rpc_client = AlloyRpcClient::new_http_with_client(http_client.clone(), u);
                let provider = RootProvider::new(rpc_client);
                Ok(ProviderState::new(provider, None, format!("provider-{i}"), url.to_string()))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(RpcClient {
            providers: Arc::new(tokio::sync::Mutex::new(providers)),
            chain_id,
            current: Arc::new(AtomicUsize::new(0)),
            dispatch_counter: Arc::new(AtomicUsize::new(0)),
        })
    }



    /// Build a shared `reqwest::Client` with gzip compression, TCP nodelay, and a request timeout.
    fn build_http_client() -> anyhow::Result<reqwest::Client> {
        reqwest::Client::builder()
            .gzip(true)
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))
    }

    /// Reset all providers to healthy state.
    pub async fn reset(&self) {
        let mut provs = self.providers.lock().await;
        for p in provs.iter_mut() {
            p.reset();
        }
        self.current.store(0, Ordering::Relaxed);
    }

    /// Returns the chain ID this client is configured for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Return a summary string of all providers and their status.
    pub async fn provider_summary(&self) -> String {
        let provs = self.providers.lock().await;
        let mut entries = Vec::new();
        for (i, p) in provs.iter().enumerate() {
            let status = if p.is_available() { "ok" } else { "dead" };
            let arch = if p.archive() { "archive" } else { "full" };
            entries.push(format!(
                "p{}[{}] {:.0}w {:.0}orig {:.1}ms {} {}",
                i, p.label(), p.weight(), p.original_weight(), p.latency_ms(), status, arch,
            ));
        }
        format!("{} providers: {}", provs.len(), entries.join("  "))
    }

    /// Returns true if at least one provider is available (not in cooldown, not dead).
    pub async fn has_healthy_providers(&self) -> bool {
        let provs = self.providers.lock().await;
        provs.iter().any(|p| p.is_available())
    }

    /// Set per-provider RPS limits. Index i maps to provider i.
    pub async fn with_provider_rps(&self, rps_list: &[f64]) {
        let mut provs = self.providers.lock().await;
        for (i, &rps) in rps_list.iter().enumerate() {
            if let Some(p) = provs.get_mut(i) {
                if rps > 0.0 {
                    p.set_rate_limiter(Some(Arc::new(RateLimiter::new(rps, rps))));
                    p.set_weight(rps);
                    p.set_original_weight(rps);
                }
            }
        }
    }

    /// Pre-seed archive capability from known endpoint metadata.
    ///
    /// For endpoints where the `archive` flag is already known (e.g. from
    /// `ProviderEndpoint.archive` in the chain catalog), this sets the flag
    /// upfront so historical `eth_call` requests route to the archive-capable
    /// provider pool first. Unknown custom endpoints default to non-archive
    /// and are served by the full-provider fallback.
    pub async fn with_provider_archive(&self, archive_list: &[bool]) {
        let mut provs = self.providers.lock().await;
        for (i, &archive) in archive_list.iter().enumerate() {
            if let Some(p) = provs.get_mut(i) {
                p.set_archive(archive);
            }
        }
    }

    /// Rotate the priority list so the next call leads with a different
    /// provider. Keeps the relative weight ordering (retries still prefer
    /// higher-weight providers) while spreading the first-try pick across all
    /// available endpoints instead of stable-sorting onto one winner.
    fn rotate_for_load(&self, mut available: Vec<(usize, ProviderState)>) -> Vec<(usize, ProviderState)> {
        if available.len() > 1 {
            let offset = self.dispatch_counter.fetch_add(1, Ordering::Relaxed) % available.len();
            available.rotate_left(offset);
        }
        available
    }

    /// Get available providers sorted by effective weight descending (fastest + highest RPS first).
    async fn sorted_available(&self) -> Vec<(usize, ProviderState)> {
        let provs = self.providers.lock().await;
        let mut available: Vec<(usize, ProviderState)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available())
            .map(|(i, p)| (i, ProviderState::clone(p)))
            .collect();

        available.sort_by(|a, b| {
            b.1.effective_weight()
                .partial_cmp(&a.1.effective_weight())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        drop(provs);
        self.rotate_for_load(available)
    }

    /// Get alive providers that can serve historical state, sorted by effective
    /// weight descending.
    ///
    /// Used by replay state reads (`eth_getBalance`, `eth_getStorageAt`,
    /// `eth_getCode`, ...). Providers that once returned "historical state is
    /// not available" are excluded so they don't waste a round-trip on every
    /// read; they remain available for block/log/fetch workloads via
    /// [`RpcClient::sorted_available`].
    async fn sorted_available_state(&self) -> Vec<(usize, ProviderState)> {
        let provs = self.providers.lock().await;
        let mut available: Vec<(usize, ProviderState)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available() && p.state_capable())
            .map(|(i, p)| (i, ProviderState::clone(p)))
            .collect();

        available.sort_by(|a, b| {
            // Prefer archive-capable providers for accurate historical state.
            let a_archive = a.1.archive() as u8;
            let b_archive = b.1.archive() as u8;
            b_archive
                .cmp(&a_archive)
                .then_with(|| {
                    b.1.effective_weight()
                        .partial_cmp(&a.1.effective_weight())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        drop(provs);
        self.rotate_for_load(available)
    }

    /// Get available **archive-capable** providers sorted by effective weight descending.
    ///
    /// Used by `retry_call(..., true)` to route archive-dependent RPC calls
    /// (`eth_getProof`, historical `eth_call`, `eth_getCode`, `eth_getStorageAt`,
    /// `eth_getBalance`, `eth_getTransactionCount`) to providers that support them.
    /// Non-archive providers are excluded — they are still alive for block/log workloads.
    async fn sorted_available_archive(&self) -> Vec<(usize, ProviderState)> {
        let provs = self.providers.lock().await;
        let mut available: Vec<(usize, ProviderState)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available() && p.archive())
            .map(|(i, p)| (i, ProviderState::clone(p)))
            .collect();

        available.sort_by(|a, b| {
            b.1.effective_weight()
                .partial_cmp(&a.1.effective_weight())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        drop(provs);
        self.rotate_for_load(available)
    }

    /// If archive-capable providers exist but are all currently in cooldown,
    /// return the shortest remaining cooldown to wait before retrying (capped
    /// at a few seconds), so `retry_call` can ride out a transient archive
    /// blip instead of bailing immediately. Returns `None` when there is no
    /// archive provider alive, or one is already available.
    async fn shortest_archive_cooldown_wait(&self) -> Option<std::time::Duration> {
        let provs = self.providers.lock().await;
        let archives: Vec<_> = provs
            .iter()
            .filter(|p| p.archive() && p.is_alive())
            .collect();
        if archives.is_empty() || archives.iter().any(|p| p.is_available()) {
            return None;
        }
        let now = tokio::time::Instant::now();
        let shortest = archives
            .iter()
            .filter_map(|p| p.cooldown_until())
            .map(|until| until.checked_duration_since(now).unwrap_or_default())
            .min()
            .unwrap_or_default();
        Some(shortest.min(std::time::Duration::from_secs(2)))
    }

    /// Execute an RPC call with per-provider rate limiting, priority selection
    /// (fastest + highest RPS first), and health tracking with exponential-backoff cooldown.
    ///
    /// When `archive_only` is true, only archive-capable providers are tried.
    /// Returns the first success or the last error if all providers fail.
    async fn retry_call<F, Fut, T>(&self, f: F, archive_only: bool) -> anyhow::Result<T>
    where
        F: Fn(RootProvider) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        self.retry_call_impl(f, archive_only, false).await
    }

    /// Like [`RpcClient::retry_call`], but routes through the state-capable
    /// provider pool only: providers that once returned "historical state is
    /// not available" are skipped. Used by replay state reads so pruned/non-
    /// archive endpoints don't waste a round-trip on every read.
    async fn retry_call_state<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: Fn(RootProvider) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        self.retry_call_impl(f, false, true).await
    }

    async fn retry_call_impl<F, Fut, T>(
        &self,
        f: F,
        archive_only: bool,
        state_req: bool,
    ) -> anyhow::Result<T>
    where
        F: Fn(RootProvider) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        // How many times to wait out a transient all-in-cooldown state before
        // giving up (each wait is capped at `MAX_COOLDOWN_WAIT`).
        const MAX_COOLDOWN_WAITS: u32 = 3;
        let mut last_err = None;
        let mut cooldown_waits = 0u32;

        let mut sorted = if state_req {
            let state = self.sorted_available_state().await;
            if state.is_empty() {
                // No provider has confirmed state capability yet this session —
                // fall back to all alive providers so the first state call can
                // still succeed and populate the capability flags.
                self.sorted_available().await
            } else {
                state
            }
        } else if archive_only {
            let arch = self.sorted_available_archive().await;
            if arch.is_empty() {
                // No archive-capable provider exists, but pruned full nodes can
                // still serve state within their retention window (typically the
                // most recent ~128 blocks). Fall back to any alive provider so
                // recent historical-state calls succeed; genuinely ancient blocks
                // will fail per-provider with a descriptive error instead.
                tracing::debug!(
                    "no archive-capable provider available — falling back to full-node providers for this state call"
                );
                self.sorted_available().await
            } else {
                arch
            }
        } else {
            self.sorted_available().await
        };
        let mut tried = std::collections::HashSet::new();

        loop {
            let mut found_next = false;
            for (idx, provider) in &sorted {
                if tried.contains(idx) {
                    continue;
                }
                // No fixed provider cap: exhaust every alive provider before
                // giving up. GetBlock shared keys inconsistently serve
                // historical state (load-balanced across nodes with different
                // retention), and state-unavailable skips are no-penalty, so
                // the last provider tried may be the only one with the state.
                if tried.len() >= sorted.len() {
                    break;
                }
                tried.insert(*idx);
                found_next = true;

                // Retry transient transport errors (e.g. fresh-connection
                // handshake resets on shared endpoints) on the same provider
                // before giving up and moving to the next one.
                let mut attempts = 0;
                let outcome = loop {
                    attempts += 1;
                    // Acquire a token before EVERY attempt (initial + retries).
                    // Retries must re-acquire so they can't burst past the
                    // configured per-provider RPS while upstream throttles us.
                    provider.acquire_permit().await;
                    let t0 = Instant::now();
                    match f(provider.provider().clone()).await {
                        Ok(val) => break Ok((val, t0.elapsed())),
                        Err(e) => {
                            let err_msg = format!("{e:#}");
                            let is_evm_revert = err_msg.contains("execution reverted");
                            let transient = !is_evm_revert && is_transport_error(&err_msg);
                            if transient && attempts < 3 {
                                tracing::debug!(
                                    "transport/rate-limit error on {} (attempt {attempts}): {err_msg}",
                                    provider.label(),
                                );
                                tokio::time::sleep(tokio::time::Duration::from_millis(300 * attempts as u64)).await;
                                continue;
                            }
                            break Err((e, is_evm_revert));
                        }
                    }
                };

                match outcome {
                    Ok((val, latency)) => {
                        let mut provs = self.providers.lock().await;
                        if let Some(p) = provs.get_mut(*idx) {
                            p.record_success(latency);
                            p.sync_rate_limiter().await;
                        }
                        self.current.store(*idx, Ordering::Relaxed);
                        return Ok(val);
                    }
                    Err((e, is_evm_revert)) => {
                        let err_msg = format!("{e:#}");
                        let rate_limited = !is_evm_revert && is_rate_limit_error(&err_msg);
                        let state_unavailable = !is_evm_revert && is_state_unavailable_error(&err_msg);
                        let mut provs = self.providers.lock().await;
                        if let Some(p) = provs.get_mut(*idx) {
                            if is_evm_revert {
                                tracing::debug!(
                                    "EVM revert on {} (expected for non-standard tokens): {}",
                                    p.label(),
                                    err_msg,
                                );
                            } else if rate_limited {
                                // Upstream throttled us (429 / quota). The endpoint is
                                // healthy — reduce its rate so the token bucket adapts
                                // below the upstream cap, with only a short cooldown.
                                p.record_rate_limited();
                                p.sync_rate_limiter().await;
                                tracing::warn!(
                                    "Rate limited on {} (rate {:.1}, cooldown {:?}): {err_msg}",
                                    p.label(),
                                    p.weight(),
                                    p.cooldown_until(),
                                );
                            } else if state_unavailable {
                                // Deterministic capability error (state pruned / not an
                                // archive node). The provider is healthy — skip it without
                                // the failure/cooldown penalty so the next provider is
                                // tried immediately and later calls aren't poisoned.
                                // Remember the capability so state reads don't re-try it.
                                p.mark_state_unavailable();
                                tracing::debug!(
                                    "State unavailable on {} (skipping, no penalty, state capability cleared): {err_msg}",
                                    p.label(),
                                );
                            } else {
                                p.record_failure();
                                p.sync_rate_limiter().await;
                                let which = if archive_only { "Archive RPC" } else { "RPC" };
                                tracing::warn!(
                                    "{} call failed on {} (failures={}, cooldown={:?}): {e:#}",
                                    which,
                                    p.label(),
                                    p.consecutive_failures(),
                                    p.cooldown_until(),
                                );
                            }
                        }
                        last_err = Some(e);
                    }
                }
            }

            if !found_next {
                // No provider could be tried this pass. If this is an archive-only
                // call and archive-capable providers exist but are all in cooldown
                // (transient 429s / connection resets), wait for the shortest
                // cooldown and retry a few times before giving up.
                if archive_only && cooldown_waits < MAX_COOLDOWN_WAITS {
                    if let Some(wait) = self.shortest_archive_cooldown_wait().await {
                        cooldown_waits += 1;
                        tracing::warn!(
                            "All archive providers in cooldown — retrying in {wait:?} (attempt {cooldown_waits}/{MAX_COOLDOWN_WAITS})"
                        );
                        tokio::time::sleep(wait).await;
                        tried.clear();
                        sorted = self.sorted_available_archive().await;
                        continue;
                    }
                }
                break;
            }

            sorted = if state_req {
                let state = self.sorted_available_state().await;
                if state.is_empty() {
                    self.sorted_available().await
                } else {
                    state
                }
            } else if archive_only {
                self.sorted_available_archive().await
            } else {
                self.sorted_available().await
            };
        }

        match last_err {
            Some(e) => {
                let which = if archive_only { "archive RPC" } else { "RPC" };
                anyhow::bail!("all {which} providers failed: {e:#}")
            }
            None => {
                if archive_only {
                    let provs = self.providers.lock().await;
                    let archive_total = provs
                        .iter()
                        .filter(|p| p.archive() && p.is_alive())
                        .count();
                    let archive_alive = provs
                        .iter()
                        .filter(|p| p.archive() && p.is_available())
                        .count();
                    drop(provs);
                    if archive_total == 0 {
                        anyhow::bail!(
                            "no archive-capable RPC provider is available — this operation requires \
                             historical state via eth_getProof. Add a genuine archive RPC endpoint \
                             (e.g. Alchemy, QuickNode, Chainstack, Infura) to the config and retry"
                        );
                    } else if archive_alive == 0 {
                        anyhow::bail!(
                            "archive-capable RPC providers exist ({archive_total}) but none are \
                             currently available (all in cooldown) — retry after the cooldown expires"
                        );
                    }
                }
                let which = if archive_only { "archive RPC" } else { "RPC" };
                anyhow::bail!("all {which} providers exhausted or in cooldown")
            }
        }
    }

    /// Distribute a block range across providers by effective weight.
    ///
    /// Returns `Vec<(usize, u64, u64)>` — (provider_index, range_start, range_end).
    /// Each provider receives blocks proportional to its effective weight
    /// (RPS adjusted by observed latency via `effective_weight()`).
    pub async fn distribute_blocks(&self, start: u64, end: u64) -> Vec<(usize, u64, u64)> {
        let total_blocks = end - start + 1;
        let provs = self.providers.lock().await;
        let alive: Vec<(usize, f64)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available())
            .map(|(i, p)| (i, p.effective_weight()))
            .collect();

        if alive.is_empty() {
            return vec![];
        }

        if alive.len() == 1 {
            return vec![(alive[0].0, start, end)];
        }

        let total_weight: f64 = alive.iter().map(|(_, w)| w).sum();

        let mut shards = Vec::new();
        let mut assigned = 0u64;
        for (idx, (provider_idx, weight)) in alive.iter().enumerate() {
            let share = if idx == alive.len() - 1 {
                total_blocks.saturating_sub(assigned)
            } else {
                let raw = (total_blocks as f64 * weight / total_weight) as u64;
                raw.max(1).min(total_blocks.saturating_sub(assigned))
            };
            let shard_start = start + assigned;
            let shard_end = shard_start + share - 1;
            shards.push((*provider_idx, shard_start, shard_end));
            assigned += share;
        }

        shards
    }

    /// Validate all providers in parallel — block number connectivity check.
    ///
    /// A provider that fails the connectivity check is marked dead; archive
    /// capability is pre-seeded from known endpoint metadata (`with_provider_archive`)
    /// and is only a routing hint for historical `eth_call`, never a blocker.
    pub async fn validate_all(&self) -> anyhow::Result<Vec<anyhow::Result<()>>> {
        // Snapshot provider labels+providers without holding the lock during validation.
        let snapshots: Vec<(usize, RootProvider, String)> = {
            let provs = self.providers.lock().await;
            provs.iter().enumerate().map(|(i, s)| (i, s.provider().clone(), s.label().to_string())).collect()
        };

        // Validate all providers concurrently.
        let validations: Vec<_> = snapshots
            .iter()
            .map(|(i, provider, label)| {
                let provider = provider.clone();
                let label = label.clone();
                let i = *i;
                async move {
                    let phase1 = Self::check_provider_connectivity(&provider, &label).await;
                    (i, label, phase1)
                }
            })
            .collect();

        let outcomes = futures::future::join_all(validations).await;

        // Apply results back under the lock.
        let mut provs = self.providers.lock().await;
        let mut results: Vec<anyhow::Result<()>> = Vec::with_capacity(provs.len());
        results.resize_with(provs.len(), || Ok(()));

        for (i, label, phase1) in outcomes {
            if let Some(state) = provs.get_mut(i) {
                if let Err(ref e) = phase1 {
                    tracing::warn!("Provider {i} ({label}) failed basic validation: {e}");
                    state.mark_dead(tokio::time::Duration::from_secs(DEAD_PROVIDER_COOLDOWN_SECS));
                    results[i] = phase1;
                    continue;
                }
                tracing::info!("{label}: OK");
            }
        }

        Ok(results)
    }

    /// Validate block number access for a provider.
    /// Returns `Err` if the provider is unreachable.
    async fn check_provider_connectivity(
        provider: &RootProvider,
        label: &str,
    ) -> anyhow::Result<()> {
        let _tip = retry_transport(|| provider.get_block_number())
            .await
            .map_err(|e| anyhow::anyhow!("{label}: eth_blockNumber failed: {e}"))?;

        Ok(())
    }

    /// Fetch the latest block number from the chain.
    pub async fn get_block_number(&self) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            provider
                .get_block_number()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
        }, false)
        .await
    }

    /// Fetch the timestamp of a specific block.
    ///
    /// Requests the full block header and extracts the timestamp.
    /// Used by `RangeResolver` for `--days` block range resolution.
    pub async fn get_block_timestamp(&self, block_number: u64) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            let block = provider
                .get_block_by_number(block_number.into())
                .hashes()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?
                .ok_or_else(|| anyhow::anyhow!("block {} not found", block_number))?;
            Ok(block.header.timestamp)
        }, false)
        .await
    }

    /// Fetch just a block's hash (header-only, no transactions).
    pub async fn get_block_hash(&self, block_number: u64) -> anyhow::Result<B256> {
        self.retry_call(|provider| async move {
            let block = provider
                .get_block_by_number(block_number.into())
                .hashes()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?
                .ok_or_else(|| anyhow::anyhow!("block {} not found", block_number))?;
            Ok(block.header.hash)
        }, false)
        .await
    }

    /// Fetch logs matching an `eth_getLogs` filter.
    ///
    /// Used for pool discovery (scanning `PairCreated` / `PoolCreated` events).
    pub async fn get_logs(&self, filter: &Filter) -> anyhow::Result<Vec<Log>> {
        self.retry_call(|provider| {
            let filter = filter.clone();
            async move {
                provider
                    .get_logs(&filter)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
        }, false)
        .await
    }

    /// Fetch logs pinned to a specific provider index, bypassing weighted random selection.
    ///
    /// Falls back to `get_logs()` on provider failure. Used by discovery to distribute
    /// `getLogs` batches across providers (like `distribute_blocks` for fetch).
    pub async fn get_logs_for(
        &self,
        provider_idx: usize,
        filter: &Filter,
    ) -> anyhow::Result<Vec<Log>> {
        let prov_state = {
            let provs = self.providers.lock().await;
            provs.get(provider_idx).cloned()
        };

        let provider = match prov_state {
            Some(p) if p.is_available() => p,
            _ => {
                return self.get_logs(filter).await;
            }
        };

        provider.acquire_permit().await;

        let t0 = Instant::now();
        let filter_clone = filter.clone();
        let result = provider
            .provider()
            .get_logs(&filter_clone)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e));
        let latency = t0.elapsed();

        match result {
            Ok(logs) => {
                let mut provs = self.providers.lock().await;
                if let Some(p) = provs.get_mut(provider_idx) {
                    p.record_success(latency);
                    p.sync_rate_limiter().await;
                }
                Ok(logs)
            }
            Err(e) => {
                tracing::warn!(
                    "Pinned provider-{} getLogs failed (failures={}, cooldown={:?}): {e:#}",
                    provider_idx,
                    provider.consecutive_failures(),
                    provider.cooldown_until(),
                );
                {
                    let mut provs = self.providers.lock().await;
                    if let Some(p) = provs.get_mut(provider_idx) {
                        p.record_failure();
                        p.sync_rate_limiter().await;
                    }
                }
                self.get_logs(filter).await
            }
        }
    }

    /// Some chains (e.g. Polygon) include non-standard transaction types (e.g. `"0x7f"`)
    /// that alloy's `TxEnvelope` cannot deserialize. This helper removes them from the raw JSON.
    fn clean_block_transactions(raw: &mut Value) {
        if let Some(transactions) = raw.get_mut("transactions") {
            if let Some(tx_array) = transactions.as_array_mut() {
                tx_array.retain(|tx| {
                    tx.get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| matches!(t, "0x0" | "0x00" | "0x01" | "0x1" | "0x02" | "0x2" | "0x03" | "0x3" | "0x04" | "0x4"))
                        .unwrap_or(true)
                });
            }
        }
    }

    fn clean_receipts(raw: &mut Value) {
        if let Some(receipts) = raw.as_array_mut() {
            receipts.retain(|r| {
                r.get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| matches!(t, "0x0" | "0x00" | "0x01" | "0x1" | "0x02" | "0x2" | "0x03" | "0x3" | "0x04" | "0x4"))
                    .unwrap_or(true)
            });
        }
    }

    /// Fetch a full block (header + transactions) by block number.
    ///
    /// Returns `BlockData` (header fields) and `Vec<TxData>` (transaction list).
    /// Transactions are converted from alloy types to internal types via `alloy_tx_to_tx_data`.
    pub async fn get_block(&self, block_number: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block: Block = self
            .retry_call(|provider| async move {
                let raw: Value = provider
                    .client()
                    .request(
                        "eth_getBlockByNumber",
                        (BlockNumberOrTag::Number(block_number), true),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                if raw.is_null() {
                    anyhow::bail!("block {} not found", block_number);
                }

                let mut raw = raw;
                Self::clean_block_transactions(&mut raw);

                serde_json::from_value::<Block>(raw).map_err(|e| anyhow::anyhow!("{}", e))
            }, false)
            .await?;

        let txs: Vec<TxData> = block
            .transactions
            .as_transactions()
            .map(|txs| {
                txs.iter()
                    .enumerate()
                    .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                    .collect()
            })
            .unwrap_or_default();

        let block_data = Self::block_to_data(&block);

        Ok((block_data, txs))
    }

    /// Fetch the pending block (header + transactions) from the node's mempool.
    ///
    /// Calls `eth_getBlockByNumber("pending", true)` to retrieve all pending
    /// (not-yet-mined) transactions. The pending block number may be `None`
    /// on some nodes — in that case `block_data.number` is set to 0.
    ///
    /// Returns an error if the RPC does not support pending block queries.
    pub async fn get_pending_block(&self) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block: Block = self
            .retry_call(|provider| async move {
                let raw: Value = provider
                    .client()
                    .request(
                        "eth_getBlockByNumber",
                        (BlockNumberOrTag::Pending, true),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                if raw.is_null() {
                    anyhow::bail!("pending block not available");
                }

                let mut raw = raw;
                Self::clean_block_transactions(&mut raw);

                serde_json::from_value::<Block>(raw).map_err(|e| anyhow::anyhow!("{}", e))
            }, false)
            .await?;

        let txs: Vec<TxData> = block
            .transactions
            .as_transactions()
            .map(|txs| {
                txs.iter()
                    .enumerate()
                    .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                    .collect()
            })
            .unwrap_or_default();

        let block_data = Self::block_to_data(&block);

        Ok((block_data, txs))
    }

    /// Fetch transaction receipts for a block.
    ///
    /// Uses `eth_getBlockReceipts` (EIP-658 receipt format).
    /// Receipts are converted from alloy types to internal types via `alloy_receipt_to_receipt_data`.
    pub async fn get_receipts(&self, block_number: u64) -> anyhow::Result<Vec<ReceiptData>> {
        let receipts = self
            .retry_call(|provider| async move {
                provider
                    .get_block_receipts(alloy::eips::BlockId::number(block_number))
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("receipts not found for block {}", block_number))
            }, false)
            .await?;

        Ok(receipts
            .iter()
            .map(alloy_receipt_to_receipt_data)
            .collect())
    }

    /// Fetch block + receipts in a single JSON-RPC batch request.
    ///
    /// Sends `eth_getBlockByNumber` and `eth_getBlockReceipts` together in one
    /// HTTP POST, cutting round-trips per block in half.
    pub async fn get_block_and_receipts_batch(
        &self,
        block_number: u64,
    ) -> anyhow::Result<(BlockData, Vec<TxData>, Vec<ReceiptData>)> {
        self.retry_call(|provider| async move {
            Self::batch_rpc_call(provider, block_number).await
        }, false)
        .await
    }

    /// Fetch block + receipts in a single JSON-RPC batch request, pinned to a specific provider.
    ///
    /// Same as `get_block_and_receipts_batch` but calls a specific provider directly,
    /// bypassing weighted random selection. Falls back to `retry_call` on failure.
    pub async fn get_block_and_receipts_batch_for(
        &self,
        provider_idx: usize,
        block_number: u64,
    ) -> anyhow::Result<(BlockData, Vec<TxData>, Vec<ReceiptData>)> {
        // Try pinned provider first
        let prov_state = {
            let provs = self.providers.lock().await;
            provs.get(provider_idx).cloned()
        };

        let provider = match prov_state {
            Some(p) if p.is_available() => p,
            _ => {
                // Pinned provider unavailable — fall back to retry_call
                return self.get_block_and_receipts_batch(block_number).await;
            }
        };

        provider.acquire_permit().await;

        let t0 = Instant::now();
        let result = Self::batch_rpc_call(provider.provider().clone(), block_number).await;
        let latency = t0.elapsed();

        match result {
            Ok(val) => {
                let mut provs = self.providers.lock().await;
                if let Some(p) = provs.get_mut(provider_idx) {
                    p.record_success(latency);
                    p.sync_rate_limiter().await;
                }
                Ok(val)
            }
            Err(e) => {
                tracing::warn!(
                    "Pinned provider-{} failed for block {} (failures={}, cooldown={:?}): {e:#}",
                    provider_idx,
                    block_number,
                    provider.consecutive_failures(),
                    provider.cooldown_until(),
                );
                {
                    let mut provs = self.providers.lock().await;
                    if let Some(p) = provs.get_mut(provider_idx) {
                        p.record_failure();
                        p.sync_rate_limiter().await;
                    }
                }
                Err(e)
            }
        }
    }

    /// Core batch RPC logic shared by pinned and unpinned paths.
    async fn batch_rpc_call(
        provider: RootProvider,
        block_number: u64,
    ) -> anyhow::Result<(BlockData, Vec<TxData>, Vec<ReceiptData>)> {
        let mut batch = BatchRequest::new(provider.client());

        let block_waiter: Waiter<Value> = batch
            .add_call(
                "eth_getBlockByNumber",
                &(BlockNumberOrTag::Number(block_number), true),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let receipts_waiter: Waiter<Value> = batch
            .add_call(
                "eth_getBlockReceipts",
                &(alloy::eips::BlockId::number(block_number),),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        batch.send().await.map_err(|e| anyhow::anyhow!("{}", e))?;

        let raw: Value = block_waiter.await.map_err(|e| anyhow::anyhow!("{}", e))?;
        if raw.is_null() {
            anyhow::bail!("block {} not found", block_number);
        }
        let mut raw = raw;
        Self::clean_block_transactions(&mut raw);
        let block_json_size = raw.to_string().len();
        let block: Block = serde_json::from_value(raw).map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut receipts_raw: Value =
            receipts_waiter.await.map_err(|e| anyhow::anyhow!("{}", e))?;
        if receipts_raw.is_null() {
            anyhow::bail!(
                "block {block_number} receipts not found (eth_getBlockReceipts returned null — \
                 the node may not have indexed this block yet)"
            );
        }
        Self::clean_receipts(&mut receipts_raw);
        let receipts_json_size = receipts_raw.to_string().len();
        let receipts: Vec<TransactionReceipt> =
            serde_json::from_value(receipts_raw).map_err(|e| anyhow::anyhow!("{}", e))?;

        tracing::debug!(
            block = block_number,
            block_bytes = block_json_size,
            receipts_bytes = receipts_json_size,
            total_bytes = block_json_size + receipts_json_size,
            "batch_rpc response sizes (pre-gzip)"
        );

        let txs: Vec<TxData> = block
            .transactions
            .as_transactions()
            .map(|txs| {
                txs.iter()
                    .enumerate()
                    .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                    .collect()
            })
            .unwrap_or_default();

        let block_data = Self::block_to_data(&block);

        Ok((block_data, txs, receipts.iter().map(alloy_receipt_to_receipt_data).collect()))
    }

    /// Fetch a single storage slot value at a historical block via `eth_getStorageAt`.
    ///
    /// Depth-unlimited — served by pruned full nodes within their retention
    /// window and by most providers at any historical depth without archive.
    /// Routed through the full provider pool (`archive_only = false`) so replay
    /// works without a genuine archive node for recent blocks.
    pub async fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block: u64,
    ) -> anyhow::Result<U256> {
        self.retry_call_state(|provider| async move {
            provider
                .get_storage_at(address, slot)
                .number(block)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .await
    }

    /// Fetch account state (nonce, balance, bytecode) at a historical block.
    ///
    /// Fires three parallel RPC calls: `eth_getTransactionCount`, `eth_getBalance`,
    /// and `eth_getCode` (via [`RpcClient::get_code`]). All three are depth-unlimited —
    /// pruned full nodes serve them within their retention window and most providers
    /// serve them at any historical depth without archive. Routed through the full
    /// provider pool so replay works without a genuine archive node.
    pub async fn get_account(
        &self,
        address: Address,
        block: u64,
    ) -> anyhow::Result<(u64, U256, Bytes)> {
        let (nonce, balance, code) = futures::try_join!(
            self.retry_call_state(|provider| async move {
                provider
                    .get_transaction_count(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }),
            self.retry_call_state(|provider| async move {
                provider
                    .get_balance(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }),
            self.get_code(address, block),
        )?;
        Ok((nonce, balance, code))
    }

    /// Fetch contract bytecode at a historical block via `eth_getCode`.
    ///
    /// Depth-unlimited — served by pruned full nodes within their retention
    /// window and by most providers at any historical depth without archive.
    /// Routed through the full provider pool so replay works without archive.
    pub async fn get_code(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        self.retry_call_state(|provider| async move {
            provider
                .get_code_at(address)
                .number(block)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .await
    }

    /// Fetch code at a historical block with no retry.
    /// Uses the first available archive provider. Still respects per-provider rate limiters.
    pub async fn get_code_no_retry(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        let first = {
            let provs = self.providers.lock().await;
            // Prefer an archive provider that can serve historical state;
            // fall back to any alive provider if none available.
            provs.iter()
                .find(|p| p.is_available() && p.archive() && p.state_capable())
                .or_else(|| provs.iter().find(|p| p.is_available() && p.state_capable()))
                .or_else(|| provs.iter().find(|p| p.is_available()))
                .cloned()
        };
        match first {
            Some(p) => {
                p.acquire_permit().await;
                p.provider()
                    .get_code_at(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
            None => anyhow::bail!("no providers available"),
        }
    }

    /// Pre-flight connection check — validates at least one provider is reachable.
    ///
    /// Checks each provider's block number access. Returns success if at least
    /// one provider passes basic connectivity.
    pub async fn check_connection(&self) -> anyhow::Result<()> {
        let results = self.validate_all().await?;
        let failures: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
            .collect();

        if failures.len() == results.len() {
            anyhow::bail!(
                "all RPC providers failed connection check:\n{}",
                failures.join("\n"),
            );
        }

        let success_count = results.len() - failures.len();
        if !failures.is_empty() {
            tracing::warn!(
                "{}/{} providers passed basic validation, {} failed:\n{}",
                success_count,
                results.len(),
                failures.len(),
                failures.join("\n"),
            );
        }

        Ok(())
    }

    /// Execute an `eth_call` at a specific block.
    async fn call_at(&self, to: Address, data: Bytes, block: BlockId) -> anyhow::Result<Bytes> {
        self.call_at_with(to, data, block, true).await
    }

    /// Execute an `eth_call` at a given block/tag, routing through the
    /// archive-capable provider pool only when `archive_only` is true.
    ///
    /// The `latest` tag is served by any full node, so `call_latest` passes
    /// `false` to avoid the "no archive-capable RPC provider" failure on
    /// full-node-only setups. Numeric historical blocks still require archive.
    async fn call_at_with(
        &self,
        to: Address,
        data: Bytes,
        block: BlockId,
        archive_only: bool,
    ) -> anyhow::Result<Bytes> {
        self.retry_call(|provider| {
            let data = data.clone();
            async move {
                let request = TransactionRequest::default()
                    .with_to(to)
                    .with_input(data);
                provider
                    .call(request)
                    .block(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
        }, archive_only)
        .await
    }

    /// Execute an `eth_call` at a historical block.
    ///
    /// Used for pool state queries (`getReserves()`, `slot0()`, `liquidity()`)
    /// without modifying chain state. Requires archive-capable providers.
    pub async fn call(&self, to: Address, data: Bytes, block: u64) -> anyhow::Result<Bytes> {
        self.call_at(to, data, BlockId::number(block)).await
    }

    /// Execute an `eth_call` at the latest block.
    ///
    /// Used for immutable metadata queries (`symbol()`, `token0()`, `token1()`,
    /// `fee()`, `tickSpacing()`) where the result never changes and archive
    /// state is not needed. Avoids `historical state not available` errors
    /// from providers without full archive support.
    pub async fn call_latest(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes> {
        self.call_at_with(to, data, BlockNumberOrTag::Latest.into(), false)
            .await
    }

    /// Fetch a single storage slot at the latest block via `eth_getStorageAt`.
    ///
    /// Works on any full node (no archive requirement) and is used by live
    /// mode's pool-state init to avoid `historical state not available` errors.
    ///
    /// Routes through the non-archive provider pool: the `latest` tag is
    /// served by every node, so this must not require archive capability.
    pub async fn get_storage_at_latest(
        &self,
        address: Address,
        slot: U256,
    ) -> anyhow::Result<U256> {
        self.retry_call(|provider| async move {
            provider
                .get_storage_at(address, slot)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
        }, false)
        .await
    }

    /// Execute an `eth_call` at either a specific block or the latest tag.
    pub async fn call_ref(&self, to: Address, data: Bytes, br: BlockRef) -> anyhow::Result<Bytes> {
        match br {
            BlockRef::Number(block) => self.call(to, data, block).await,
            BlockRef::Latest => self.call_latest(to, data).await,
        }
    }

    /// Fetch a storage slot at either a specific block or the latest tag.
    pub async fn get_storage_at_ref(
        &self,
        address: Address,
        slot: U256,
        br: BlockRef,
    ) -> anyhow::Result<U256> {
        match br {
            BlockRef::Number(block) => self.get_storage_at(address, slot, block).await,
            BlockRef::Latest => self.get_storage_at_latest(address, slot).await,
        }
    }

    fn block_to_data(block: &Block) -> BlockData {
        BlockData {
            number: block.header.number,
            hash: block.header.hash,
            timestamp: block.header.timestamp,
            base_fee_per_gas: block.header.base_fee_per_gas.map(|v| v as u128),
            gas_limit: block.header.gas_limit,
            gas_used: block.header.gas_used,
            coinbase: block.header.beneficiary,
            difficulty: block.header.difficulty,
            mix_hash: block.header.mix_hash,
        }
    }

    /// Detect the state horizon — the earliest block number for which the RPC
    /// providers can still serve historical state (balance, storage, code).
    ///
    /// Performs a binary search between `tip.saturating_sub(max_depth)` and `tip`,
    /// probing `eth_getBalance` on a well-known address (WETH on each chain).
    /// Returns the earliest block where state is available, or `tip - 100`
    /// (conservative fallback for full nodes with standard retention).
    ///
    /// The result is suitable for deciding per-block whether full EVM replay
    /// (`run_block`) or log-only processing (`sync_block_from_logs`) should
    /// be used in the hybrid backtest path.
    pub async fn detect_state_horizon(&self, tip: u64) -> u64 {
        const MAX_DEPTH: u64 = 2000;
        const PROBE_ADDRESS: alloy::primitives::Address = alloy::primitives::address!(
            "d0e1139178bc088d7467266f75993d5164f4b058"
        );

        let lo = tip.saturating_sub(MAX_DEPTH);
        let hi = tip.saturating_sub(1).max(1);

        // Quick smoke test: can we get state at `hi` at all?
        if self.get_balance(PROBE_ADDRESS, hi).await.is_err() {
            tracing::warn!(
                "State unavailable at block {hi} — falling back to tip - 100"
            );
            return tip.saturating_sub(100);
        }

        // Binary search: find the lowest block where state IS available.
        let mut lo = lo;
        let mut hi = hi;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.get_balance(PROBE_ADDRESS, mid).await {
                Ok(_) => hi = mid,
                Err(_) => lo = mid + 1,
            }
        }

        tracing::info!(
            "State horizon detected: block {lo} (tip={tip}, depth={})",
            tip - lo
        );
        lo
    }

    /// Fetch account balance at a historical block.
    ///
    /// Routed through the state-capable provider pool so pruned endpoints
    /// are skipped for historical-state calls.
    async fn get_balance(
        &self,
        address: alloy::primitives::Address,
        block: u64,
    ) -> anyhow::Result<alloy::primitives::U256> {
        self.retry_call_state(|provider| async move {
            provider
                .get_balance(address)
                .number(block)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .await
    }

}

/// Extract the first 4 bytes of transaction calldata as a method selector.
/// Returns `None` if input is shorter than 4 bytes (plain ETH transfer or CREATE).
pub(crate) fn extract_selector(input: &Bytes) -> Option<[u8; 4]> {
    if input.len() >= 4 {
        let mut sel = [0u8; 4];
        sel.copy_from_slice(&input[..4]);
        Some(sel)
    } else {
        None
    }
}

fn alloy_tx_to_tx_data(tx: &AlloyTx, index: u64) -> TxData {
    TxData {
        hash: *tx.inner.hash(),
        index,
        tx_type: tx.inner.tx_type() as u8,
        from: tx.inner.signer(),
        to: tx.inner.to(),
        input: tx.inner.input().clone(),
        value: tx.inner.value(),
        gas_limit: tx.inner.gas_limit(),
        max_fee_per_gas: tx.inner.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.inner.max_priority_fee_per_gas(),
        nonce: tx.inner.nonce(),
        access_list: tx
            .inner
            .access_list()
            .map(|al| {
                al.iter()
                    .map(|item| AccessListItem {
                        address: item.address,
                        slots: item.storage_keys.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        authorization_list: tx
            .inner
            .authorization_list()
            .map(|al| {
                al.iter()
                    .map(|a| crate::data::AuthorizationData {
                        chain_id: *a.inner().chain_id(),
                        address: *a.inner().address(),
                        nonce: a.inner().nonce(),
                        y_parity: a.y_parity(),
                        r: a.r(),
                        s: a.s(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn alloy_receipt_to_receipt_data(receipt: &TransactionReceipt) -> ReceiptData {
    ReceiptData {
        tx_hash: receipt.transaction_hash,
        tx_index: receipt.transaction_index.unwrap_or(0),
        status: receipt.status(),
        gas_used: receipt.gas_used,
        cumulative_gas_used: receipt.inner.cumulative_gas_used(),
        logs: receipt
            .logs()
            .iter()
            .map(|l| LogData {
                address: l.address(),
                topics: l.topics().to_vec(),
                data: l.data().data.clone(),
            })
            .collect(),
        contract_address: receipt.contract_address,
    }
}

/// Recommended eth_getLogs batch size for a set of RPC URLs.
///
/// Alchemy free-tier endpoints cap `eth_getLogs` at ~10 blocks per request.
/// This function returns 100 if any URL contains "alchemy.com", otherwise the
/// caller's default. The lower batch avoids noisy retry warnings during the
/// adaptive `probe_get_logs_limit` phase.
pub fn recommended_get_logs_batch(urls: &[String], default: u64) -> u64 {
    if urls.iter().any(|u| u.contains("alchemy.com")) {
        100.min(default)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::{is_rate_limit_error, is_transport_error};

    #[test]
    fn classifies_http_429_as_transient() {
        for msg in [
            "HTTP error 429 with empty body",
            "HTTP error 429",
            "rate limit exceeded",
            "too many requests",
            "request was throttled",
            "HTTP error 402 with body: Out of CU",
        ] {
            assert!(is_rate_limit_error(msg), "should detect rate limit: {msg}");
            assert!(is_transport_error(msg), "429/quota should be retryable: {msg}");
        }
    }

    #[test]
    fn classifies_connection_failures_as_transient() {
        for msg in [
            "error sending request for url (https://example.com)",
            "error sending request",
            "connection reset by peer",
            "connection refused",
            "connection closed",
            "timed out",
            "operation timed out",
        ] {
            assert!(is_transport_error(msg), "should be transient: {msg}");
        }
    }

    #[test]
    fn does_not_retry_definitive_errors() {
        for msg in [
            "execution reverted",
            "missing trie node 0xabc (state not available)",
            "invalid argument: hex string",
            "chain ID mismatch: got 137, expected 43114",
            "VM Exception while processing transaction: revert",
        ] {
            assert!(!is_transport_error(msg), "should not be transient: {msg}");
        }
    }
}


