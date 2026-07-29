//! Rate-limiting and provider health tracking for the RPC layer.

use std::sync::Arc;

use alloy::providers::RootProvider;
use crate::rpc::consts::MAX_BACKOFF_SECS;

/// Token-bucket rate limiter for throttling RPC requests.
///
/// Maintains a token bucket that refills at `rate` tokens per second.
/// Each `acquire()` call consumes one token, blocking until one is available.
/// Up to `burst` tokens can accumulate for short bursts.
///
/// Thread-safe and designed for shared use across concurrent tasks.
#[derive(Debug)]
pub struct RateLimiter {
    state: tokio::sync::Mutex<RateLimiterState>,
}

#[derive(Debug)]
struct RateLimiterState {
    tokens: f64,
    last_refill: tokio::time::Instant,
    rate: f64,
    burst: f64,
}

impl RateLimiter {
    pub fn new(rate: f64, burst: f64) -> Self {
        let effective_burst = burst.max(1.0);
        Self {
            state: tokio::sync::Mutex::new(RateLimiterState {
                tokens: effective_burst,
                last_refill: tokio::time::Instant::now(),
                rate,
                burst: effective_burst,
            }),
        }
    }

    /// Acquire one token, blocking until available.
    pub async fn acquire(&self) {
        loop {
            let sleep_dur = {
                let mut state = self.state.lock().await;
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(state.last_refill).as_secs_f64();
                state.tokens = (state.tokens + elapsed * state.rate).min(state.burst);
                state.last_refill = now;

                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }

                let deficit = 1.0 - state.tokens;
                tokio::time::Duration::from_secs_f64(deficit / state.rate)
            };
            tokio::time::sleep(sleep_dur).await;
        }
    }

    /// Adjust the token-refill rate and burst capacity at runtime.
    ///
    /// Used by adaptive backoff: when a provider hits errors, its RPS is
    /// reduced; when it recovers, the rate is gradually restored.
    pub async fn set_rate(&self, new_rate: f64) {
        let mut state = self.state.lock().await;
        let clamped = new_rate.max(0.1);
        state.rate = clamped;
        state.burst = clamped.max(1.0);
    }
}

/// Tracks health and rate-limiting state for a single RPC provider.
#[derive(Debug, Clone)]
pub struct ProviderState {
    provider: RootProvider,
    rate_limiter: Option<Arc<RateLimiter>>,
    weight: f64,
    original_weight: f64,
    is_alive: bool,
    cooldown_until: Option<tokio::time::Instant>,
    consecutive_failures: u64,
    latency_ms: f64,
    label: String,
    url: String,
    /// Whether this provider supports archive queries (`eth_getProof`, historical `eth_call`, etc.).
    /// Set during `validate_all`: `true` if the `eth_getProof` probe succeeds, `false` otherwise.
    /// Non-archive providers are still alive for block/log/fetch workloads.
    archive: bool,
}

impl ProviderState {
    pub fn new(provider: RootProvider, rps: Option<f64>, label: String, url: String) -> Self {
        let r = rps.unwrap_or(1.0).max(0.1);
        let rate_limiter = rps.map(|_| Arc::new(RateLimiter::new(r, r)));
        Self {
            provider,
            rate_limiter,
            weight: r,
            original_weight: r,
            is_alive: true,
            cooldown_until: None,
            consecutive_failures: 0,
            latency_ms: 0.0,
            label,
            url,
            archive: true,
        }
    }

    // ── Accessors ──────────────────────────────────────────────────────

    pub fn provider(&self) -> &RootProvider {
        &self.provider
    }

    pub fn rate_limiter(&self) -> Option<&Arc<RateLimiter>> {
        self.rate_limiter.as_ref()
    }

    pub fn weight(&self) -> f64 {
        self.weight
    }

    pub fn original_weight(&self) -> f64 {
        self.original_weight
    }

    pub fn is_alive(&self) -> bool {
        self.is_alive
    }

    pub fn cooldown_until(&self) -> Option<tokio::time::Instant> {
        self.cooldown_until
    }

    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures
    }

    pub fn latency_ms(&self) -> f64 {
        self.latency_ms
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn archive(&self) -> bool {
        self.archive
    }

    // ── Mutators ───────────────────────────────────────────────────────

    pub fn set_archive(&mut self, archive: bool) {
        self.archive = archive;
    }

    pub fn set_weight(&mut self, weight: f64) {
        self.weight = weight;
    }

    pub fn set_original_weight(&mut self, weight: f64) {
        self.original_weight = weight;
    }

    pub fn set_rate_limiter(&mut self, rl: Option<Arc<RateLimiter>>) {
        self.rate_limiter = rl;
    }

    /// Mark provider as failed (not dead). Sets `is_alive = false` without
    /// changing cooldown or weight. Use when a provider should be excluded
    /// but without the exponential-backoff penalty of `record_failure`.
    pub fn mark_failed(&mut self) {
        self.is_alive = false;
    }

    /// Reset provider to a healthy state: alive, no cooldown, no failures.
    /// Does not change weight, latency, or rate-limiter configuration.
    pub fn reset(&mut self) {
        self.is_alive = true;
        self.cooldown_until = None;
        self.consecutive_failures = 0;
    }

    /// Mark provider as completely dead with an explicit cooldown. Used when
    /// validation fails (e.g. wrong chain ID, unreachable endpoint). The
    /// provider is excluded from distribution until the cooldown expires
    /// or a successful RPC call resets it via `record_success()`.
    pub fn mark_dead(&mut self, cooldown: tokio::time::Duration) {
        self.is_alive = false;
        self.consecutive_failures += 1;
        self.cooldown_until = Some(tokio::time::Instant::now() + cooldown);
        self.weight = (self.weight * 0.5).max(self.original_weight * 0.1);
    }

    // ── Behaviour ──────────────────────────────────────────────────────

    pub fn is_available(&self) -> bool {
        if !self.is_alive {
            return false;
        }
        match self.cooldown_until {
            Some(until) => tokio::time::Instant::now() >= until,
            None => true,
        }
    }

    pub fn record_success(&mut self, latency: std::time::Duration) {
        self.consecutive_failures = 0;
        self.is_alive = true;
        self.cooldown_until = None;
        self.latency_ms = self.latency_ms * 0.8 + latency.as_secs_f64() * 1000.0 * 0.2;
        self.weight = (self.weight * 1.5).min(self.original_weight);
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        let backoff_secs = 2u64.saturating_pow(self.consecutive_failures as u32).min(MAX_BACKOFF_SECS);
        self.cooldown_until =
            Some(tokio::time::Instant::now() + tokio::time::Duration::from_secs(backoff_secs));
        self.weight = (self.weight * 0.5).max(self.original_weight * 0.1);
    }

    /// Sync the rate limiter's token-bucket rate to match the current adaptive weight.
    ///
    /// Must be called after `record_failure()` or `record_success()` to propagate
    /// weight changes to the actual token-bucket throughput.
    pub async fn sync_rate_limiter(&self) {
        if let Some(rl) = &self.rate_limiter {
            rl.set_rate(self.weight).await;
        }
    }

    /// Acquire a rate-limiter token if configured.
    pub async fn acquire_permit(&self) {
        if let Some(rl) = &self.rate_limiter {
            rl.acquire().await;
        }
    }

    /// Compute effective weight combining configured RPS with observed latency.
    ///
    /// Faster providers (lower latency) naturally receive more blocks.
    /// Falls back to raw `weight` when no latency data is available yet.
    pub fn effective_weight(&self) -> f64 {
        if self.latency_ms <= 0.0 {
            return self.weight.max(0.1);
        }
        (self.weight / self.latency_ms.sqrt()).max(0.1)
    }
}
