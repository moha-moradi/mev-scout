//! Token metadata cache — avoids redundant `symbol()` eth_call RPC calls
//! and stores optional name / icon URL from remote enrichers.
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

/// One cached token row (symbol required; name / icon optional).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedToken {
    pub symbol: String,
    pub decimals: Option<i32>,
    pub name: Option<String>,
    pub icon_url: Option<String>,
}

impl CachedToken {
    pub fn symbol_only(symbol: String, decimals: Option<i32>) -> Self {
        Self {
            symbol,
            decimals,
            name: None,
            icon_url: None,
        }
    }

    /// Fill empty fields from `other` without overwriting richer local data.
    pub fn merge_from(&mut self, other: &CachedToken) {
        if self.symbol.is_empty() && !other.symbol.is_empty() {
            self.symbol = other.symbol.clone();
        }
        if self.decimals.is_none() {
            self.decimals = other.decimals;
        }
        if self.name.is_none() {
            self.name = other.name.clone();
        }
        if self.icon_url.is_none() {
            self.icon_url = other.icon_url.clone();
        }
    }

    pub fn needs_llama_enrichment(&self) -> bool {
        self.symbol.is_empty() || self.decimals.is_none()
    }

    pub fn needs_coingecko_enrichment(&self) -> bool {
        self.name.is_none() || self.icon_url.is_none()
    }
}

/// In-memory token metadata cache backed by SQLite.
#[derive(Debug, Clone, Default)]
pub struct TokenCache {
    /// address → metadata
    inner: HashMap<Address, CachedToken>,
}

impl TokenCache {
    /// Load all cached token symbols from SQLite into memory.
    pub fn load(store: &SqliteStore) -> anyhow::Result<Self> {
        let conn = store.conn();
        // Prefer the enriched columns; fall back if a very old DB somehow
        // skipped migration (should not happen after SCHEMA_VERSION bump).
        let mut stmt = conn.prepare(
            "SELECT address, symbol, decimals, name, icon_url FROM token_symbols",
        )?;

        let rows = stmt.query_map([], |row| {
            let addr_bytes: Vec<u8> = row.get(0)?;
            let symbol: String = row.get(1)?;
            let decimals: Option<i32> = row.get(2)?;
            let name: Option<String> = row.get(3)?;
            let icon_url: Option<String> = row.get(4)?;
            Ok((addr_bytes, symbol, decimals, name, icon_url))
        })?;

        let mut inner = HashMap::new();
        let mut count = 0u64;
        for row in rows {
            let (addr_bytes, symbol, decimals, name, icon_url) = row?;
            if addr_bytes.len() == 20 {
                let addr = Address::from_slice(&addr_bytes);
                inner.insert(
                    addr,
                    CachedToken {
                        symbol,
                        decimals,
                        name,
                        icon_url,
                    },
                );
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
                inner.insert(addr, CachedToken::symbol_only(w.symbol.clone(), Some(w.decimals)));
            }
        }
        if inner.is_empty() {
            if let Ok(addr) = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse::<Address>() {
                inner.insert(addr, CachedToken::symbol_only("WETH".to_string(), Some(18)));
            }
        }

        for t in &data.always_warm {
            if let Ok(addr) = t.address.parse::<Address>() {
                inner.entry(addr).or_insert_with(|| {
                    CachedToken::symbol_only(t.symbol.clone(), Some(t.decimals))
                });
            }
        }

        tracing::info!(
            "Token cache: pre-populated with {} known symbols",
            inner.len()
        );
        TokenCache { inner }
    }

    /// Look up a cached symbol for a token address.
    #[inline]
    pub fn get(&self, addr: &Address) -> Option<&str> {
        self.inner.get(addr).map(|t| t.symbol.as_str())
    }

    /// Look up the full cached row.
    #[inline]
    pub fn get_meta(&self, addr: &Address) -> Option<&CachedToken> {
        self.inner.get(addr)
    }

    /// Insert or replace a token row in the in-memory cache.
    pub fn insert(&mut self, addr: Address, symbol: String, decimals: Option<i32>) {
        self.inner
            .insert(addr, CachedToken::symbol_only(symbol, decimals));
    }

    /// Insert or merge a full metadata row.
    pub fn upsert_meta(&mut self, addr: Address, meta: CachedToken) {
        match self.inner.get_mut(&addr) {
            Some(existing) => existing.merge_from(&meta),
            None => {
                self.inner.insert(addr, meta);
            }
        }
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

    /// Bulk-save newly resolved tokens to SQLite.
    /// Skips tokens that are already cached in memory.
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
            let addr_bytes: &[u8] = addr.0.as_slice();
            conn.execute(
                "INSERT OR REPLACE INTO token_symbols (address, symbol, decimals, name, icon_url) \
                 VALUES (?1, ?2, ?3, NULL, NULL)",
                rusqlite::params![addr_bytes, symbol, decimals],
            )?;
            saved += 1;
        }
        if saved > 0 {
            tracing::info!("Token cache: saved {} new symbols to SQLite", saved);
        }
        Ok(saved)
    }

    /// Persist the full in-memory cache to SQLite (upsert every row).
    pub fn persist_all(&self, store: &SqliteStore) -> anyhow::Result<u64> {
        let conn = store.conn();
        let mut saved = 0u64;
        for (addr, meta) in &self.inner {
            if meta.symbol.is_empty() {
                continue;
            }
            let addr_bytes: &[u8] = addr.0.as_slice();
            conn.execute(
                "INSERT INTO token_symbols (address, symbol, decimals, name, icon_url) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(address) DO UPDATE SET \
                   symbol=excluded.symbol, \
                   decimals=COALESCE(excluded.decimals, token_symbols.decimals), \
                   name=COALESCE(excluded.name, token_symbols.name), \
                   icon_url=COALESCE(excluded.icon_url, token_symbols.icon_url)",
                rusqlite::params![
                    addr_bytes,
                    meta.symbol,
                    meta.decimals,
                    meta.name,
                    meta.icon_url
                ],
            )?;
            saved += 1;
        }
        if saved > 0 {
            tracing::info!("Token cache: persisted {} token row(s) to SQLite", saved);
        }
        Ok(saved)
    }

    /// Merge another cache into this one (fill-only).
    pub fn merge(&mut self, other: TokenCache) {
        for (addr, meta) in other.inner {
            self.upsert_meta(addr, meta);
        }
    }

    /// Return all cached entries (for serialization or inspection).
    pub fn entries(&self) -> &HashMap<Address, CachedToken> {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn merge_from_fills_gaps_only() {
        let mut a = CachedToken {
            symbol: "USDC".into(),
            decimals: Some(6),
            name: None,
            icon_url: None,
        };
        let b = CachedToken {
            symbol: "OTHER".into(),
            decimals: Some(18),
            name: Some("USD Coin".into()),
            icon_url: Some("https://example.com/usdc.png".into()),
        };
        a.merge_from(&b);
        assert_eq!(a.symbol, "USDC");
        assert_eq!(a.decimals, Some(6));
        assert_eq!(a.name.as_deref(), Some("USD Coin"));
        assert_eq!(a.icon_url.as_deref(), Some("https://example.com/usdc.png"));
    }

    #[test]
    fn persist_and_load_roundtrip_name_icon() {
        let path = std::env::temp_dir().join(format!(
            "mev-scout-tok-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        let mut cache = TokenCache::default();
        let addr = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        cache.upsert_meta(
            addr,
            CachedToken {
                symbol: "USDC".into(),
                decimals: Some(6),
                name: Some("USD Coin".into()),
                icon_url: Some("https://example.com/usdc.png".into()),
            },
        );
        cache.persist_all(&store).unwrap();

        let loaded = TokenCache::load(&store).unwrap();
        let meta = loaded.get_meta(&addr).unwrap();
        assert_eq!(meta.symbol, "USDC");
        assert_eq!(meta.decimals, Some(6));
        assert_eq!(meta.name.as_deref(), Some("USD Coin"));
        assert_eq!(
            meta.icon_url.as_deref(),
            Some("https://example.com/usdc.png")
        );
        let _ = std::fs::remove_file(&path);
    }
}
