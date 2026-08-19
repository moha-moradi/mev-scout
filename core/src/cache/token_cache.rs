//! Token symbol cache — avoids redundant `symbol()` eth_call RPC calls.
//!
//! Tokens are cached in SQLite (persistent) and loaded into a HashMap for
//! O(1) lookups during pool discovery. Newly resolved symbols are saved
//! back to SQLite after each discovery run.
//!
//! Pre-populated with well-known tokens per chain to minimize cold-start
//! RPC calls.

use std::collections::HashMap;

use alloy::primitives::Address;
use serde::Deserialize;

use super::store::SqliteStore;

#[derive(Deserialize)]
struct TokenInfo {
    symbol: String,
    decimals: i32,
    address: String,
}

#[derive(Deserialize)]
struct KnownTokens {
    wrapped_native_by_chain: HashMap<String, TokenInfo>,
    always_warm: Vec<TokenInfo>,
}

/// In-memory token symbol cache backed by SQLite.
///
/// Usage:
/// ```ignore
/// let cache = TokenCache::load(&store)?;
/// let symbol = cache.get(&token_address);  // O(1) lookup, no RPC
/// cache.save(&store, address, "USDC", Some(6))?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct TokenCache {
    /// address → (symbol, decimals)
    inner: HashMap<Address, (String, Option<i32>)>,
}

impl TokenCache {
    /// Load all cached token symbols from SQLite into memory.
    pub fn load(store: &SqliteStore) -> anyhow::Result<Self> {
        let conn = store.conn();
        let mut stmt = conn.prepare(
            "SELECT address, symbol, decimals FROM token_symbols"
        )?;

        let rows = stmt.query_map([], |row| {
            let addr_bytes: Vec<u8> = row.get(0)?;
            let symbol: String = row.get(1)?;
            let decimals: Option<i32> = row.get(2)?;
            Ok((addr_bytes, symbol, decimals))
        })?;

        let mut inner = HashMap::new();
        let mut count = 0u64;
        for row in rows {
            let (addr_bytes, symbol, decimals) = row?;
            if addr_bytes.len() == 20 {
                let addr = Address::from_slice(&addr_bytes);
                inner.insert(addr, (symbol, decimals));
                count += 1;
            }
        }

        tracing::info!("Token cache: loaded {} cached symbols from SQLite", count);
        Ok(TokenCache { inner })
    }

    /// Create a new empty cache and pre-populate with well-known tokens.
    pub fn warm(chain_id: u64) -> Self {
        let data: KnownTokens = serde_json::from_str(include_str!("../../data/known_tokens.json"))
            .expect("invalid known_tokens.json");

        let mut inner = HashMap::new();

        if let Some(w) = data.wrapped_native_by_chain.get(&chain_id.to_string()) {
            if let Ok(addr) = w.address.parse::<Address>() {
                inner.insert(addr, (w.symbol.clone(), Some(w.decimals)));
            }
        }
        if inner.is_empty() {
            if let Ok(addr) = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse::<Address>() {
                inner.insert(addr, ("WETH".to_string(), Some(18)));
            }
        }

        for t in &data.always_warm {
            if let Ok(addr) = t.address.parse::<Address>() {
                inner.entry(addr).or_insert_with(|| (t.symbol.clone(), Some(t.decimals)));
            }
        }

        tracing::info!("Token cache: pre-populated with {} known symbols", inner.len());
        TokenCache { inner }
    }

    /// Look up a cached symbol for a token address.
    #[inline]
    pub fn get(&self, addr: &Address) -> Option<&str> {
        self.inner.get(addr).map(|(s, _)| s.as_str())
    }

    /// Look up cached (symbol, decimals) for a token address.
    #[inline]
    pub fn get_full(&self, addr: &Address) -> Option<(&str, Option<i32>)> {
        self.inner.get(addr).map(|(s, d)| (s.as_str(), *d))
    }

    /// Insert a new token symbol into the in-memory cache.
    pub fn insert(&mut self, addr: Address, symbol: String, decimals: Option<i32>) {
        self.inner.insert(addr, (symbol, decimals));
    }

    /// Check if a token address is already cached.
    #[inline]
    pub fn contains(&self, addr: &Address) -> bool {
        self.inner.contains_key(addr)
    }

    /// Return the number of cached tokens.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the cache is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return token addresses that are NOT in the cache (need RPC resolution).
    pub fn missing(&self, addrs: &[Address]) -> Vec<Address> {
        addrs.iter()
            .filter(|a| !self.inner.contains_key(*a))
            .copied()
            .collect()
    }

    /// Bulk-save newly resolved tokens to SQLite.
    /// Skips tokens that are already cached.
    pub fn save_batch(
        &self,
        store: &SqliteStore,
        tokens: &[(Address, String, Option<i32>)],
    ) -> anyhow::Result<u64> {
        let conn = store.conn();
        let mut saved = 0u64;
        for (addr, symbol, decimals) in tokens {
            if self.inner.contains_key(addr) {
                continue;
            }
            let addr_bytes: &[u8] = &addr.0.as_slice();
            conn.execute(
                "INSERT OR REPLACE INTO token_symbols (address, symbol, decimals) VALUES (?1, ?2, ?3)",
                rusqlite::params![addr_bytes, symbol, decimals],
            )?;
            saved += 1;
        }
        if saved > 0 {
            tracing::info!("Token cache: saved {} new symbols to SQLite", saved);
        }
        Ok(saved)
    }

    /// Persist every token in the in-memory cache to SQLite (no skip).
    pub fn save_all_to_sqlite(&self, store: &SqliteStore) -> anyhow::Result<u64> {
        let conn = store.conn();
        let mut saved = 0u64;
        for (addr, (symbol, decimals)) in &self.inner {
            let addr_bytes: &[u8] = addr.0.as_slice();
            conn.execute(
                "INSERT OR REPLACE INTO token_symbols (address, symbol, decimals) VALUES (?1, ?2, ?3)",
                rusqlite::params![addr_bytes, symbol, decimals],
            )?;
            saved += 1;
        }
        if saved > 0 {
            tracing::info!("Token cache: persisted {} tokens to SQLite", saved);
        }
        Ok(saved)
    }

    /// Save a single token to both in-memory cache and SQLite.
    pub fn save_one(
        &mut self,
        store: &SqliteStore,
        addr: Address,
        symbol: &str,
        decimals: Option<i32>,
    ) -> anyhow::Result<()> {
        let conn = store.conn();
        let addr_bytes: &[u8] = addr.0.as_slice();
        conn.execute(
            "INSERT OR REPLACE INTO token_symbols (address, symbol, decimals) VALUES (?1, ?2, ?3)",
            rusqlite::params![addr_bytes, symbol, decimals],
        )?;
        self.inner.insert(addr, (symbol.to_string(), decimals));
        Ok(())
    }

    /// Merge another cache into this one.
    pub fn merge(&mut self, other: TokenCache) {
        for (addr, (symbol, decimals)) in other.inner {
            self.inner.entry(addr).or_insert((symbol, decimals));
        }
    }

    /// Return all cached entries (for serialization or inspection).
    pub fn entries(&self) -> &HashMap<Address, (String, Option<i32>)> {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_filter() {
        let mut cache = TokenCache::default();
        let addr1 = "0x0000000000000000000000000000000000000001".parse().unwrap();
        let addr2 = "0x0000000000000000000000000000000000000002".parse().unwrap();
        let addr3 = "0x0000000000000000000000000000000000000003".parse().unwrap();

        cache.insert(addr1, "USDC".to_string(), Some(6));
        cache.insert(addr3, "WETH".to_string(), Some(18));

        let missing = cache.missing(&[addr1, addr2, addr3]);
        assert_eq!(missing, vec![addr2]);
    }
}
