use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::pool::dex_type::DexType;
use crate::pool::state::{PoolManager, PoolState};

/// An edge in the price graph — a single pool that connects two tokens.
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub address: Address,
    pub dex_type: DexType,
    pub token_a: Address,
    pub token_b: Address,
    /// V2: reserve of token_a
    pub reserve_a: u128,
    /// V2: reserve of token_b
    pub reserve_b: u128,
    /// V3: sqrt price
    pub sqrt_price_x96: Option<U256>,
    /// V3: current tick
    pub tick: Option<i32>,
    /// V3: current liquidity
    pub liquidity: Option<u128>,
    pub fee_bps: u32,
    pub tick_spacing: Option<u32>,
    pub is_concentrated_liquidity: bool,
}

impl GraphEdge {
    pub fn other_token(&self, token: &Address) -> Option<Address> {
        if *token == self.token_a {
            Some(self.token_b)
        } else if *token == self.token_b {
            Some(self.token_a)
        } else {
            None
        }
    }
}

/// A directed price graph built from the pool manager.
///
/// Nodes are token addresses. Edges are pools.
pub struct PriceGraph {
    /// token address → list of (edge_index, is_token_a)
    token_index: HashMap<Address, Vec<(usize, bool)>>,
    edges: Vec<GraphEdge>,
}

impl PriceGraph {
    pub fn new() -> Self {
        PriceGraph {
            token_index: HashMap::new(),
            edges: Vec::new(),
        }
    }

    /// Rebuild the graph from the current pool manager state.
    pub fn rebuild(&mut self, pm: &PoolManager) {
        self.token_index.clear();
        self.edges.clear();

        for pool in pm.all_pools() {
            let info = pool.info();
            let edge = match pool {
                PoolState::UniswapV2(s) => GraphEdge {
                    address: s.info.address,
                    dex_type: DexType::UniswapV2,
                    token_a: s.info.token0,
                    token_b: s.info.token1,
                    reserve_a: s.reserve0,
                    reserve_b: s.reserve1,
                    sqrt_price_x96: None,
                    tick: None,
                    liquidity: None,
                    fee_bps: s.info.fee,
                    tick_spacing: None,
                    is_concentrated_liquidity: false,
                },
                PoolState::UniswapV3(s) => GraphEdge {
                    address: s.info.address,
                    dex_type: DexType::UniswapV3,
                    token_a: s.info.token0,
                    token_b: s.info.token1,
                    reserve_a: 0,
                    reserve_b: 0,
                    sqrt_price_x96: Some(s.sqrt_price_x96),
                    tick: Some(s.tick),
                    liquidity: Some(s.liquidity),
                    fee_bps: s.info.fee,
                    tick_spacing: s.info.tick_spacing,
                    is_concentrated_liquidity: true,
                },
                PoolState::Curve(s) => {
                    // Curve: map first two tokens as token_a/b for graph purposes
                    let tokens: Vec<Address> = s.token_index.iter().map(|(a, _)| *a).collect();
                    let token_a = tokens.first().copied().unwrap_or(Address::ZERO);
                    let token_b = tokens.get(1).copied().unwrap_or(Address::ZERO);
                    let bal_a = s.balances.first().copied().unwrap_or(0);
                    let bal_b = s.balances.get(1).copied().unwrap_or(0);
                    GraphEdge {
                        address: s.info.address,
                        dex_type: DexType::Curve,
                        token_a,
                        token_b,
                        reserve_a: bal_a,
                        reserve_b: bal_b,
                        sqrt_price_x96: None,
                        tick: None,
                        liquidity: None,
                        fee_bps: s.info.fee,
                        tick_spacing: None,
                        is_concentrated_liquidity: false,
                    }
                }
                PoolState::Balancer(s) => {
                    let tokens: Vec<Address> = s.token_index.iter().map(|(a, _)| *a).collect();
                    let token_a = tokens.first().copied().unwrap_or(Address::ZERO);
                    let token_b = tokens.get(1).copied().unwrap_or(Address::ZERO);
                    let bal_a = s.balances.first().copied().unwrap_or(0);
                    let bal_b = s.balances.get(1).copied().unwrap_or(0);
                    GraphEdge {
                        address: s.info.address,
                        dex_type: DexType::Balancer,
                        token_a,
                        token_b,
                        reserve_a: bal_a,
                        reserve_b: bal_b,
                        sqrt_price_x96: None,
                        tick: None,
                        liquidity: None,
                        fee_bps: s.info.fee,
                        tick_spacing: None,
                        is_concentrated_liquidity: false,
                    }
                }
            };

            let idx = self.edges.len();
            self.edges.push(edge);

            self.token_index
                .entry(info.token0)
                .or_default()
                .push((idx, true));
            self.token_index
                .entry(info.token1)
                .or_default()
                .push((idx, false));
        }
    }

    /// Find all edges that connect two tokens, optionally filtering by DEX type.
    pub fn find_edges(&self, token_a: &Address, token_b: &Address) -> Vec<&GraphEdge> {
        let candidates = match self.token_index.get(token_a) {
            Some(v) => v,
            None => return Vec::new(),
        };
        let mut result = Vec::new();
        for &(idx, _) in candidates {
            let edge = &self.edges[idx];
            if edge.other_token(token_a) == Some(*token_b) {
                result.push(edge);
            }
        }
        result
    }

    /// Find all edges that connect two tokens (owned version).
    pub fn find_edges_owned(&self, token_a: Address, token_b: Address) -> Vec<GraphEdge> {
        self.find_edges(&token_a, &token_b)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Return all distinct token addresses in the graph.
    pub fn all_tokens(&self) -> HashSet<Address> {
        self.token_index.keys().copied().collect()
    }

    /// Return all edges.
    pub fn all_edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// Number of edges (pools).
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Find all pairs of pools that share a common token (for arbitrage detection).
    /// Returns (pool_1_address, pool_2_address, shared_token).
    pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
        let mut pairs = Vec::new();
        let mut seen = HashSet::new();

        for (_token, indices) in &self.token_index {
            for i in 0..indices.len() {
                for j in (i + 1)..indices.len() {
                    let e1 = &self.edges[indices[i].0];
                    let e2 = &self.edges[indices[j].0];
                    let a = e1.address;
                    let b = e2.address;
                    let key = if a < b { (a, b) } else { (b, a) };
                    if seen.insert(key) {
                        pairs.push((key.0, key.1, *_token));
                    }
                }
            }
        }

        pairs
    }
}

impl Default for PriceGraph {
    fn default() -> Self {
        Self::new()
    }
}
