//! Verifies the RPC rate-limiter actually caps throughput as configured.
//!
//! Test 1 is self-contained (token bucket math, no network).
//! Tests 2/3 need a live endpoint; set `MEV_SCOUT_TEST_RPC` to a URL.
//!   - Test 2: client throttled to 15 RPS -> 40 calls must take >= ~1.4s, no errors.
//!   - Test 3: client with NO rate limiter -> raw endpoint burst tolerance (may error).

#![allow(clippy::unwrap_used, clippy::expect_used)]
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
    std::env::var("MEV_SCOUT_TEST_RPC")
        .ok()
        .filter(|s| !s.is_empty())
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

fn offline_client(urls: &[&str]) -> RpcClient {
    RpcClient::from_urls(urls, 43_114).unwrap()
}

#[tokio::test]
async fn unlimited_provider_takes_almost_all_blocks() {
    let rpc = offline_client(&["http://127.0.0.1:9", "http://127.0.0.1:10"]);
    rpc.with_provider_rps(&[0.0, 1.0]).await;
    let shards = rpc.distribute_blocks(0, 9_999).await;
    assert_eq!(shards.len(), 2);
    let local = shards.iter().find(|s| s.provider_idx == 0).unwrap();
    let public = shards.iter().find(|s| s.provider_idx == 1).unwrap();
    let local_n = local.to - local.from + 1;
    let public_n = public.to - public.from + 1;
    assert!(local_n > 9_000, "local shard {local_n}");
    assert!(public_n < 500, "public shard {public_n}");
    assert!(rpc.provider_is_unlimited(0).await);
    assert!(!rpc.provider_is_unlimited(1).await);
}

#[tokio::test]
async fn positive_rps_splits_blocks_proportionally() {
    let rpc = offline_client(&[
        "http://127.0.0.1:9",
        "http://127.0.0.1:10",
        "http://127.0.0.1:11",
        "http://127.0.0.1:12",
    ]);
    rpc.with_provider_rps(&[20.0, 20.0, 1.0, 1.0]).await;
    let shards = rpc.distribute_blocks(0, 4_199).await;
    assert_eq!(shards.len(), 4);
    let n = |idx: usize| {
        let s = shards.iter().find(|s| s.provider_idx == idx).unwrap();
        s.to - s.from + 1
    };
    assert_eq!(n(0), 2_000);
    assert_eq!(n(1), 2_000);
    assert_eq!(n(2), 100);
    assert_eq!(n(3), 100);
}

#[tokio::test]
async fn mixed_positive_rps_round_robins_first_try() {
    let rpc = offline_client(&[
        "http://127.0.0.1:9",
        "http://127.0.0.1:10",
        "http://127.0.0.1:11",
        "http://127.0.0.1:12",
    ]);
    rpc.with_provider_rps(&[20.0, 20.0, 1.0, 1.0]).await;
    let mut firsts = std::collections::HashSet::new();
    for _ in 0..16 {
        let order = rpc.any_scope_try_order().await;
        assert_eq!(order.len(), 4);
        firsts.insert(order[0]);
    }
    assert_eq!(
        firsts.len(),
        4,
        "every positive-RPS provider must lead a first try, got {firsts:?}"
    );
}

#[tokio::test]
async fn unlimited_peer_is_always_the_first_try() {
    let rpc = offline_client(&["http://127.0.0.1:9", "http://127.0.0.1:10"]);
    rpc.with_provider_rps(&[0.0, 1.0]).await;
    for _ in 0..8 {
        let order = rpc.any_scope_try_order().await;
        assert_eq!(order.first().copied(), Some(0));
        assert!(order.contains(&1), "rate-limited peer stays as failover");
    }
}
