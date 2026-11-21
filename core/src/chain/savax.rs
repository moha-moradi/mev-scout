//! Benqi sAVAX exchange-rate reads shared by the explorer and `live`.
//!
//! One `eth_call` per UTC hour per process: a success is reused for the hour
//! and a failure is remembered too (public nodes prune historical state, and
//! retrying on every block serializes a backfill). The explorer keys the cache
//! by block timestamp; `live` passes wall-clock seconds, which are the same
//! hour near the tip.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, Bytes, U256};

use crate::rpc::RpcClient;

/// One hour in seconds (`ts / 3600` bucket key).
pub fn hour_bucket(ts: u64) -> u64 {
    ts / 3600
}

/// `getPooledAvaxByShares(1e18)` calldata → AVAX wad per 1 sAVAX.
pub fn rate_call_data() -> Bytes {
    let mut data = crate::pool::selectors::SAVAX_GET_POOLED_AVAX_BY_SHARES.to_vec();
    data.extend_from_slice(&U256::from(10u64.pow(18)).to_be_bytes::<32>());
    Bytes::from(data)
}

/// sAVAX exchange-rate reads keyed by [`hour_bucket`].
#[derive(Debug, Default)]
pub struct SavaxRateCache {
    by_hour: HashMap<u64, U256>,
    missed: HashSet<u64>,
}

impl SavaxRateCache {
    pub fn get(&self, hour: u64) -> Option<U256> {
        self.by_hour.get(&hour).copied()
    }

    pub fn insert(&mut self, hour: u64, rate: U256) {
        self.missed.remove(&hour);
        self.by_hour.insert(hour, rate);
    }

    pub fn missed(&self, hour: u64) -> bool {
        self.missed.contains(&hour)
    }

    pub fn miss(&mut self, hour: u64) {
        self.missed.insert(hour);
    }
}

/// Benqi sAVAX `getPooledAvaxByShares(1e18)` → AVAX wad per 1 sAVAX (plan P3.16).
///
/// `ts` selects the hourly cache bucket. Returns `None` when sAVAX is not
/// configured, the call fails, or it already failed this hour.
pub async fn fetch_savax_exchange_rate(
    rpc: &RpcClient,
    savax: Option<Address>,
    block: u64,
    ts: u64,
    cache: &mut SavaxRateCache,
) -> Option<U256> {
    let savax = savax.filter(|a| !a.is_zero())?;
    let hour = hour_bucket(ts);
    if let Some(rate) = cache.get(hour) {
        return Some(rate);
    }
    if cache.missed(hour) {
        return None;
    }
    match rpc.call(savax, rate_call_data(), block).await {
        Ok(ret) if ret.len() >= 32 => {
            let rate = U256::from_be_slice(&ret[..32]);
            cache.insert(hour, rate);
            Some(rate)
        }
        Ok(_) => {
            cache.miss(hour);
            None
        }
        Err(e) => {
            tracing::debug!(block, "sAVAX exchangeRate eth_call failed: {e}");
            cache.miss(hour);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_utc_hours() {
        assert_eq!(hour_bucket(3_600), 1);
        assert_eq!(hour_bucket(3_599), 0);
    }

    #[test]
    fn cache_reuses_success_and_remembers_miss() {
        let mut cache = SavaxRateCache::default();
        cache.insert(10, U256::from(7u64));
        assert_eq!(cache.get(10), Some(U256::from(7u64)));
        cache.miss(11);
        assert!(cache.missed(11));
        assert!(cache.get(11).is_none());
        cache.insert(11, U256::from(9u64));
        assert!(!cache.missed(11), "a success clears the miss marker");
    }

    #[test]
    fn call_data_carries_one_share() {
        let data = rate_call_data();
        assert_eq!(data.len(), 4 + 32);
        assert_eq!(data[4..], U256::from(10u64.pow(18)).to_be_bytes::<32>());
    }
}
