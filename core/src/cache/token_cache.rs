//! Token symbol cache — avoids redundant `symbol()` eth_call RPC calls.
//!
//! Tokens are cached in SQLite (persistent) and loaded into a HashMap for
//! O(1) lookups during pool discovery. Newly resolved symbols are saved
//! back to SQLite after each discovery run.
//!
//! Pre-populated with well-known tokens per chain to minimize cold-start
//! RPC calls. Dune Analytics can also bulk-populate the cache.

use std::collections::HashMap;

use alloy::primitives::Address;

use super::store::SqliteStore;
use crate::dune::client::DuneClient;
use crate::dune::pool_discovery::dune_chain_label;
use crate::dune::queries::QUERY_ALL_TOKENS;

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

    /// Fetch all known tokens from Dune Analytics and merge into the cache.
    ///
    /// Executes `QUERY_ALL_TOKENS` against Dune, which returns every ERC-20
    /// token registered on the chain in `tokens.erc20`. Newly discovered
    /// tokens are persisted to SQLite so future runs avoid the Dune call.
    ///
    /// Dune free-tier allows ~1,000 executions/hour, so this should be called
    /// only once per discovery session (e.g. on cold start).
    pub async fn fetch_from_dune(
        &mut self,
        client: &DuneClient,
        chain: &str,
    ) -> anyhow::Result<u64> {
        let chain_label = dune_chain_label(chain);
        let sql = QUERY_ALL_TOKENS.replace("{chain}", &chain_label);
        tracing::info!("Token cache: querying Dune for all {} tokens...", chain_label);

        let result = client.execute_raw_sql(&sql).await?;
        let rows = match result.result {
            Some(r) => r.rows,
            None => {
                tracing::warn!("Token cache: Dune returned no results");
                return Ok(0);
            }
        };

        let total = rows.len();
        let mut new_count = 0u64;
        for row in &rows {
            let addr = match row.get("contract_address").and_then(|v| v.as_str()) {
                Some(s) => match s.parse::<Address>() {
                    Ok(a) => a,
                    Err(_) => continue,
                },
                None => continue,
            };
            let symbol = match row.get("symbol").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let decimals = row.get("decimals").and_then(|v| {
                if let Some(n) = v.as_u64() {
                    Some(n as i32)
                } else if let Some(s) = v.as_str() {
                    s.parse::<i32>().ok()
                } else {
                    None
                }
            });

            if !self.inner.contains_key(&addr) {
                self.inner.insert(addr, (symbol, decimals));
                new_count += 1;
            }
        }

        tracing::info!(
            "Token cache: fetched {} tokens from Dune ({} new, {} already cached)",
            total, new_count, total - new_count as usize
        );
        Ok(new_count)
    }

    /// Create a new empty cache and pre-populate with well-known tokens.
    pub fn warm(chain_id: u64) -> Self {
        let mut inner = HashMap::new();

        // Common wrapped native tokens
        let wrapped_native = match chain_id {
            1    => ("WETH",  18, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            10   => ("WETH",  18, "0x4200000000000000000000000000000000000006"),
            56   => ("WBNB",  18, "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c"),
            137  => ("WPOL",  18, "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270"),
            42161 => ("WETH", 18, "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            43114 => ("WAVAX",18, "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"),
            8453  => ("WETH", 18, "0x4200000000000000000000000000000000000006"),
            _     => ("WETH", 18, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        };
        if let Ok(addr) = wrapped_native.2.parse::<Address>() {
            inner.insert(addr, (wrapped_native.0.to_string(), Some(wrapped_native.1)));
        }

        // Major stablecoins
        let stables: &[(&str, i32, &str)] = &[
            // USDC
            ("USDC", 6, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),  // Ethereum
            ("USDC", 6, "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"),  // Polygon (bridged)
            ("USDC", 6, "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),  // Polygon (native)
            ("USDC.e", 6, "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"), // Arbitrum
            ("USDC", 6, "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),  // Base
            ("USDC", 6, "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85"),  // Optimism
            ("USDC.e", 6, "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E"), // Avalanche
            // USDT
            ("USDT", 6, "0xdAC17F958D2ee523a2206206994597C13D831ec7"),  // Ethereum
            ("USDT", 6, "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"),  // Polygon
            ("USDT", 6, "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"),  // Arbitrum
            ("USDT", 6, "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2"),  // Base
            ("USDT", 6, "0x94b008aA00579c1307B0EF2c499aD98a8ce58e58"),  // Optimism
            ("USDT.e", 6, "0xc7198437980c041389c85c43e942b31037ADb125"), // Avalanche
            // DAI
            ("DAI", 18, "0x6B175474E89094C44Da98b954EedeAC495271d0F"),  // Ethereum
            ("DAI", 18, "0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063"),  // Polygon
            ("DAI", 18, "0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1"),  // Arbitrum
            ("DAI", 18, "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb"),  // Base
            ("DAI", 18, "0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1"),  // Optimism
            ("DAI.e", 18, "0xd586E7F844cEa2F50bf48A84c291e3C71F0fDa99"), // Avalanche
        ];
        for (sym, dec, addr_str) in stables {
            if let Ok(addr) = addr_str.parse::<Address>() {
                inner.entry(addr).or_insert_with(|| (sym.to_string(), Some(*dec)));
            }
        }

        // Major tokens (multi-chain)
        let majors: &[(&str, i32, &str)] = &[
            ("WBTC", 8, "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),  // Ethereum
            ("WBTC", 8, "0x1BFD67037B42Cf73acF204719844fc479E662510"),  // Polygon
            ("WBTC", 8, "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f"),  // Arbitrum
            ("WBTC", 8, "0x68f180fcCe6836688e9084f035309E29Bf0A2095"),  // Base
            ("WBTC", 8, "0x68f180fcCe6836688e9084f035309E29Bf0A2095"),  // Optimism
            ("WBTC.e", 8, "0x50b7545627a5162F82A992c33b87aDc75187B218"), // Avalanche
            ("QUICK", 18, "0x1FD188D040B1457bA878CB776D34bA332840f702"), // Polygon (QuickSwap)
            ("AAVE", 18, "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9"),  // Ethereum
            ("LINK", 18, "0x514910771AF9Ca656af840dff83E8264EcF986CA"),  // Ethereum
            ("LINK", 18, "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39"),  // Polygon
            ("UNI", 18, "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984"),  // Ethereum
            ("CRV", 18, "0xD533a949740bb3306d119CC777fa900bA034cd52"),  // Ethereum
            ("CRV", 18, "0x172370d5Cd63279eFa6d502Dab29171933a610AF"),  // Polygon
            ("WMATIC", 18, "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270"), // Polygon (alias)
            ("BAL", 18, "0xba100000625a3754423978a60c9317c58a424e3D"),  // Ethereum
            ("SUSHI", 18, "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2"),  // Ethereum
            ("SUSHI", 18, "0x0b3F86A235AEc7424616818A8af15aDc172Cf580"),  // Polygon
            ("COMP", 18, "0xc00e94Cb662C3520282E6f5717214004A7f26888"),  // Ethereum
            ("MKR", 18, "0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2"),  // Ethereum
            ("SNX", 18, "0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F"),  // Ethereum
            ("LDO", 18, "0x5A98FcBEA516Cf06857215779Fd812CA3beF1B32"),  // Ethereum
            ("ENS", 18, "0xC18360217D8F7Ab5e7c516566761Ea12Ce7F9D72"),  // Ethereum
            ("RPL", 18, "0xD33526068D116c6E47163D64144803178950399F"),  // Ethereum
            ("GRT", 18, "0xc944E90C64B2c07662A292be6244BDf05Cda44a7"),  // Ethereum
            ("FET", 18, "0x1D207E85335D92a511fa229d51030dfe324B862A"),  // Ethereum
        ];
        for (sym, dec, addr_str) in majors {
            if let Ok(addr) = addr_str.parse::<Address>() {
                inner.entry(addr).or_insert_with(|| (sym.to_string(), Some(*dec)));
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

    /// Merge another cache into this one (e.g., from Dune query results).
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
