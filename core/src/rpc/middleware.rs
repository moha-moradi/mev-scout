//! Rate-limiting and provider health tracking for the RPC layer.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use alloy::providers::RootProvider;

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
    rate: f64,
    burst: f64,
}

#[derive(Debug)]
struct RateLimiterState {
    tokens: f64,
    last_refill: tokio::time::Instant,
}

impl RateLimiter {
    pub fn new(rate: f64, burst: f64) -> Self {
        Self {
            state: tokio::sync::Mutex::new(RateLimiterState {
                tokens: burst,
                last_refill: tokio::time::Instant::now(),
            }),
            rate,
            burst,
        }
    }

    /// Acquire one token, blocking until available.
    pub async fn acquire(&self) {
        loop {
            let sleep_dur = {
                let mut state = self.state.lock().await;
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(state.last_refill).as_secs_f64();
                state.tokens = (state.tokens + elapsed * self.rate).min(self.burst);
                state.last_refill = now;

                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }

                let deficit = 1.0 - state.tokens;
                tokio::time::Duration::from_secs_f64(deficit / self.rate)
            };
            tokio::time::sleep(sleep_dur).await;
        }
    }
}

/// ETag cache for HTTP conditional requests.
///
/// Tracks ETag values per URL so subsequent requests can include
/// `If-None-Match` headers. When a server responds with 304 Not Modified,
/// the cached response can be reused without re-downloading.
///
/// Particularly useful for free RPC providers that support ETags, as it
/// reduces bandwidth and can bypass some throttling mechanisms.
#[derive(Debug, Clone, Default)]
pub struct EtagStore {
    inner: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
}

impl EtagStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the stored ETag for a URL, if any.
    pub async fn get_etag(&self, url: &str) -> Option<String> {
        self.inner.read().await.get(url).cloned()
    }

    /// Store an ETag for a URL.
    pub async fn set_etag(&self, url: &str, etag: String) {
        self.inner.write().await.insert(url.to_string(), etag);
    }

    /// Number of tracked URLs.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Whether the store is empty.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

/// Tracks health and rate-limiting state for a single RPC provider.
#[derive(Debug, Clone)]
pub struct ProviderState {
    pub provider: RootProvider,
    pub rate_limiter: Option<Arc<RateLimiter>>,
    pub weight: f64,
    pub is_alive: bool,
    pub cooldown_until: Option<Instant>,
    pub consecutive_failures: u64,
    pub latency_ms: f64,
    pub label: String,
    pub url: String,
}

impl ProviderState {
    pub fn new(provider: RootProvider, rps: Option<f64>, label: String, url: String) -> Self {
        let rate_limiter = rps.map(|r| Arc::new(RateLimiter::new(r.max(0.1), r.max(0.1))));
        Self {
            provider,
            rate_limiter,
            weight: rps.unwrap_or(1.0),
            is_alive: true,
            cooldown_until: None,
            consecutive_failures: 0,
            latency_ms: 0.0,
            label,
            url,
        }
    }

    pub fn is_available(&self) -> bool {
        if !self.is_alive {
            return false;
        }
        match self.cooldown_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }

    pub fn record_success(&mut self, latency: std::time::Duration) {
        self.consecutive_failures = 0;
        self.is_alive = true;
        self.cooldown_until = None;
        self.latency_ms = self.latency_ms * 0.8 + latency.as_secs_f64() * 1000.0 * 0.2;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        let backoff_secs = 2u64.saturating_pow(self.consecutive_failures as u32).min(300);
        self.cooldown_until = Some(Instant::now() + std::time::Duration::from_secs(backoff_secs));
    }

    /// Mark provider as completely dead. Used when validation fails (e.g. wrong
    /// chain ID, unreachable endpoint). The provider is excluded from distribution
    /// until a successful RPC call resets it via `record_success()`.
    pub fn mark_dead(&mut self) {
        self.is_alive = false;
        self.record_failure();
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
