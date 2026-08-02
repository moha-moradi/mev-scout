//! Verifies the RPC rate-limiter actually caps throughput as configured.
//!
//! Test 1 is self-contained (token bucket math, no network).
//! Tests 2/3 need a live endpoint; set `MEV_SCOUT_TEST_RPC` to a URL.
//!   - Test 2: client throttled to 15 RPS -> 40 calls must take >= ~1.4s, no errors.
//!   - Test 3: client with NO rate limiter -> raw endpoint burst tolerance (may error).

use std::time::{Duration, Instant};

use mev_scout_core::rpc::{RateLimiter, RpcClient};

#[test]
fn rate_limiter_respects_configured_rps() {
    let limiter = std::sync::Arc::new(RateLimiter::new(15.0, 15.0));
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let t0 = Instant::now();
    runtime.block_on(async {
        let mut handles = Vec::new();
        for _ in 0..30 {
            let l = limiter.clone();
            handles.push(tokio::spawn(async move {
                l.acquire().await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    });
    let elapsed = t0.elapsed();

    // 30 tokens: first 15 come from the burst instantly, remaining 15 at 15/s -> >= 1.0s.
    assert!(
        elapsed >= Duration::from_millis(900),
        "rate limiter too fast: 30 requests at 15 RPS finished in {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(8),
        "rate limiter unexpectedly slow: {elapsed:?}"
    );
    println!("rate_limiter_respects_configured_rps: 30 req @15RPS took {elapsed:?}");
}

fn test_rpc_url() -> Option<String> {
    std::env::var("MEV_SCOUT_TEST_RPC").ok().filter(|s| !s.is_empty())
}

async fn run_burst(rpc: &RpcClient, count: usize) -> (usize, usize, Duration, Vec<String>) {
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..count {
        let r = rpc.clone();
        handles.push(tokio::spawn(async move {
            match r.get_block_number().await {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("{e:#}")),
            }
        }));
    }
    let mut ok = 0usize;
    let mut errs = Vec::new();
    for h in handles {
        match h.await.unwrap_or(Err("task panicked".into())) {
            Ok(()) => ok += 1,
            Err(e) => errs.push(e),
        }
    }
    (ok, count - ok, t0.elapsed(), errs)
}

#[tokio::test]
async fn client_throttles_at_configured_rps() {
    let Some(url) = test_rpc_url() else {
        eprintln!("SKIP: MEV_SCOUT_TEST_RPC not set");
        return;
    };
    let rpc = RpcClient::new(&url, 137).unwrap();
    rpc.with_provider_rps(&[15.0]).await;

    let (ok, fail, elapsed, errs) = run_burst(&rpc, 40).await;

    println!(
        "client_throttles_at_configured_rps (limiter=15): ok={ok} fail={fail} elapsed={elapsed:?} ({:.1} rps)",
        ok as f64 / elapsed.as_secs_f64()
    );
    for e in errs.iter().take(6) {
        println!("  throttled error: {e}");
    }
    assert_eq!(fail, 0, "requests failed while throttled to 15 RPS");
    assert!(
        elapsed >= Duration::from_millis(1400),
        "rate limiter not capping: 40 req @15RPS finished in {elapsed:?}"
    );
}

#[tokio::test]
async fn client_unlimited_endpoint_burst_tolerance() {
    let Some(url) = test_rpc_url() else {
        eprintln!("SKIP: MEV_SCOUT_TEST_RPC not set");
        return;
    };
    let rpc = RpcClient::new(&url, 137).unwrap();

    let (ok, fail, elapsed, errs) = run_burst(&rpc, 40).await;

    println!(
        "client_unlimited_endpoint_burst_tolerance (no limiter): ok={ok} fail={fail} elapsed={elapsed:?} ({:.1} rps)",
        ok as f64 / elapsed.as_secs_f64()
    );
    for e in errs.iter().take(6) {
        println!("  unlimited error: {e}");
    }
    // Informational: raw endpoint burst tolerance. No assertion on errors —
    // the endpoint may legitimately cap concurrent/rapid requests.
}
