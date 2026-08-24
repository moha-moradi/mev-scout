use crate::data::ExecutedLog;
use crate::pool::decoders;
use crate::pool::math::consts::LIQUIDITY_CHANGE_THRESHOLD_DIVISOR;
use crate::pool::state::apply::SWAP_TOPIC;
use crate::pool::state::pool_types::{
    is_fee_on_transfer_token, is_rebase_token, PoolState, UniswapV2PoolState, UniswapV3PoolState,
};
use crate::utils::u128_from_be_bytes;
use alloy::primitives::{b256, Address, B256, U256};
use std::cmp;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// ERC20 `Transfer(address,address,uint256)` topic0.
const ERC20_TRANSFER_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
/// Relative shortfall between a swap's declared amount and what the token
/// contract actually transferred beyond which the token is flagged taxed (#9).
/// Well above any modeled DEX fee, so only off-invariant shortfalls trip it.
const TAX_SHORTFALL_MARGIN: f64 = 0.05;
/// Declared amounts below this are ignored when checking tax shortfalls
/// (dust rounding noise).
const MIN_TAX_CHECK_AMOUNT: u128 = 1_000;

/// Actual ERC20 transfer totals per (pool, token), split by direction.
type TransferTotals = HashMap<(Address, Address), u128>;
/// One swap's declared amounts: (pool, token_in, amount_in, token_out, amount_out).
type SwapDeclaration = (Address, Option<Address>, u128, Option<Address>, u128);

/// Detection scan scope for incremental arbitrage scanning.
///
/// Pool states untouched since the last detection pass cannot produce *new*
/// opportunities, so after the first full scan of a block only pairs containing
/// at least one dirty (recently-updated) pool need to be re-checked.
pub enum ScanScope<'a> {
    /// Scan every arbitrage pair (first detection pass of a block).
    Full,
    /// Restrict to pairs containing at least one pool in the given dirty set.
    Dirty(&'a HashSet<Address>),
}

impl<'a> ScanScope<'a> {
    /// Whether the pair (a, b) is in scope for this scan.
    pub fn contains_pair(&self, a: &Address, b: &Address) -> bool {
        match self {
            ScanScope::Full => true,
            ScanScope::Dirty(dirty) => dirty.contains(a) || dirty.contains(b),
        }
    }
}

/// Manages runtime pool state for all tracked pools during block replay.
///
/// Responsibilities:
/// - Stores and updates `PoolState` for every pool in the registry
/// - Maintains a `token_index` for fast token→pool lookups (used by arb detectors)
/// - Caches computed arbitrage pairs (invalidated on `add_pool`)
/// - Dispatches on-chain event logs to the appropriate state update method
///
/// `PoolManager` is the single source of truth for pool state during a run.
#[derive(Debug)]
pub struct PoolManager {
    pub(crate) pools: HashMap<Address, PoolState>,
    /// token address -> list of pool addresses that trade this token
    pub(crate) token_index: HashMap<Address, Vec<Address>>,
    /// Cached arbitrage pairs (invalidated on add_pool). Shared via `Arc` so
    /// per-block detection can clone the handle instead of the whole Vec.
    pub(crate) pairs_cache: Mutex<Option<Arc<Vec<(Address, Address, Address)>>>>,
    /// Pools whose state changed since the last `take_dirty_pools()` call.
    /// Used to restrict per-transaction detection to affected pairs only.
    pub(crate) dirty_pools: HashSet<Address>,
    /// Tokens learned to be transfer-taxed at runtime (#9): declared swap
    /// amounts exceeded what the ERC20 contract actually moved. Session-only.
    pub(crate) dynamic_fot: HashSet<Address>,
    /// Address of the wrapped native token (WMATIC/WETH/WBNB) per chain.
    pub(crate) wrapped_native: Option<Address>,
    /// Address of the Balancer V2 vault for flash loans and pool state queries.
    pub(crate) balancer_vault: Option<Address>,
    /// Pre-filter set of known pool addresses for fast log filtering.
    pub(crate) known_set: HashSet<Address>,
    /// Maximum number of pools per token when computing arbitrage pairs.
    pub(crate) max_pairs_per_token: usize,
    /// Per-token overrides for max_pairs_per_token (H3).
    /// Allows configuring different caps for high/medium/low-connectivity tokens.
    /// Key = token address, value = per-token max pairs limit.
    pub(crate) token_max_pairs: HashMap<Address, usize>,
    /// Maximum number of concurrent RPC calls during pool initialization.
    pub(crate) concurrency_limit: u32,
    /// When true, pool-state init queries use the `latest` block tag instead of
    /// a numeric block. Served by any full node (no archive requirement) — used
    /// by live mode. Backtest/replay keep numeric blocks for historical state.
    pub(crate) use_latest: bool,
}

impl PoolManager {
    /// Create an empty pool manager with no pools loaded.
    ///
    /// Pools must be added via `add_pool()` and initialized via `init_from_rpc()`
    /// before use in detection.
    pub fn new() -> Self {
        PoolManager {
            pools: HashMap::new(),
            token_index: HashMap::new(),
            pairs_cache: Mutex::new(None),
            dirty_pools: HashSet::new(),
            dynamic_fot: HashSet::new(),
            wrapped_native: None,
            balancer_vault: None,
            known_set: HashSet::new(),
            max_pairs_per_token: 50,
            token_max_pairs: HashMap::new(),
            concurrency_limit: 1,
            use_latest: false,
        }
    }

    /// Create a pool manager pre-allocated for the given number of pools.
    pub fn with_capacity(capacity: usize) -> Self {
        PoolManager {
            pools: HashMap::with_capacity(capacity),
            token_index: HashMap::with_capacity(capacity),
            pairs_cache: Mutex::new(None),
            dirty_pools: HashSet::with_capacity(capacity),
            dynamic_fot: HashSet::with_capacity(capacity),
            wrapped_native: None,
            balancer_vault: None,
            known_set: HashSet::with_capacity(capacity),
            max_pairs_per_token: 50,
            token_max_pairs: HashMap::new(),
            concurrency_limit: 1,
            use_latest: false,
        }
    }

    /// Set the maximum number of pool pairs per token for arbitrage pair computation.
    pub fn set_max_pairs_per_token(&mut self, max: usize) {
        self.max_pairs_per_token = max;
    }

    /// Set per-token max pairs limit (H3). Tokens without an explicit override
    /// use the global `max_pairs_per_token`. Set 0 for no limit.
    pub fn set_token_max_pairs(&mut self, token: Address, max: usize) {
        self.token_max_pairs.insert(token, max);
    }

    /// Get the effective max_pairs for a given token, accounting for per-token overrides.
    fn effective_max_pairs(&self, token: &Address) -> usize {
        self.token_max_pairs
            .get(token)
            .copied()
            .unwrap_or(self.max_pairs_per_token)
    }

    /// Set the maximum number of concurrent RPC calls during pool initialization.
    /// Lower values (1-3) are safer for public RPCs with rate limits.
    pub fn set_concurrency_limit(&mut self, limit: u32) {
        self.concurrency_limit = limit.max(1);
    }

    /// Set whether pool-state init should query the `latest` block tag instead of
    /// a numeric block (archive-free, for live mode).
    pub fn set_use_latest(&mut self, use_latest: bool) {
        self.use_latest = use_latest;
    }

    /// Builder variant of [`Self::set_use_latest`].
    pub fn with_use_latest(mut self, use_latest: bool) -> Self {
        self.set_use_latest(use_latest);
        self
    }

    /// Add a pool and update the token index.
    ///
    /// Invalidates the cached arbitrage pairs (recomputed on next `arbitrage_pairs()` call).
    /// Skips ZERO addresses in token index to avoid polluting pair computation.
    pub fn add_pool(&mut self, state: PoolState) {
        let addr = state.address();
        let info = state.info().clone();
        self.known_set.insert(addr);
        self.pools.insert(addr, state);
        if !info.token0.is_zero() {
            self.token_index.entry(info.token0).or_default().push(addr);
        }
        if !info.token1.is_zero() {
            self.token_index.entry(info.token1).or_default().push(addr);
        }
        *self.pairs_cache.lock().expect("pairs_cache mutex poisoned") = None;
    }

    /// Look up a pool by address.
    pub fn get(&self, address: &Address) -> Option<&PoolState> {
        self.pools.get(address)
    }

    /// Mutable lookup — used to update reserves after events.
    pub fn get_mut(&mut self, address: &Address) -> Option<&mut PoolState> {
        self.pools.get_mut(address)
    }

    /// Iterate over all tracked pools.
    pub fn all_pools(&self) -> impl Iterator<Item = &PoolState> {
        self.pools.values()
    }

    /// Number of pools currently tracked.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// All pool addresses (used for transaction filtering during replay).
    pub fn pool_addresses(&self) -> Vec<Address> {
        self.pools.keys().copied().collect()
    }

    /// Returns all unique token addresses tracked by the pool manager.
    ///
    /// Used by the transaction filter in `replay.rs` to determine whether
    /// a transaction touches any tracked token (and thus needs full EVM replay).
    pub fn token_addresses(&self) -> Vec<Address> {
        self.token_index.keys().copied().collect()
    }

    /// Returns all pool addresses that trade the given token.
    pub fn pools_for_token(&self, token: &Address) -> Option<&[Address]> {
        self.token_index.get(token).map(|v| v.as_slice())
    }

    /// Find a pool that trades both `token_a` and `token_b`.
    ///
    /// Typically used to find a WMATIC pair for USD pricing fallback.
    /// Returns the first match found in the token index.
    pub fn find_pair_pool(&self, token_a: &Address, token_b: &Address) -> Option<Address> {
        let pools_a = self.token_index.get(token_a)?;
        let pools_b = self.token_index.get(token_b)?;
        // Find the first address common to both sets
        // Use the smaller set for iteration
        let (smaller, larger) = if pools_a.len() < pools_b.len() {
            (pools_a, pools_b)
        } else {
            (pools_b, pools_a)
        };
        smaller.iter().find(|addr| larger.contains(addr)).copied()
    }

    /// Estimate a pool's total liquidity/TVL for sorting purposes.
    /// Higher values mean more meaningful arbitrage candidates.
    pub fn pool_liquidity_estimate(&self, addr: &Address) -> u128 {
        match self.pools.get(addr) {
            Some(PoolState::UniswapV2(v2)) => {
                // Use the smaller reserve as a conservative liquidity bound
                v2.reserve0.min(v2.reserve1)
            }
            Some(PoolState::UniswapV3(v3)) => v3.liquidity,
            Some(PoolState::UniswapV4(v4)) => v4.liquidity,
            Some(PoolState::Curve(c)) => c.balances.iter().sum(),
            Some(PoolState::Balancer(b)) => b.balances.iter().sum(),
            Some(PoolState::TraderJoeLB(lb)) => lb.reserve_x.min(lb.reserve_y),
            Some(PoolState::Pendle(p)) => p.total_pt.min(p.total_sy),
            None => 0,
        }
    }

    /// Returns pairs of pool addresses that share at least one common token.
    /// Each pair is returned once (pool_a < pool_b by address), with the shared token.
    /// Pools are sorted by liquidity estimate (descending) before truncation to
    /// `max_pairs_per_token`, so high-volume pairs are preferred over low-volume ones.
    /// Result is cached behind an `Arc` and invalidated on add_pool; callers clone
    /// the handle (cheap) rather than the whole pair list.
    pub fn arbitrage_pairs(&self) -> Arc<Vec<(Address, Address, Address)>> {
        if let Some(cached) = &*self.pairs_cache.lock().expect("pairs_cache mutex poisoned") {
            return Arc::clone(cached);
        }
        let mut pairs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (_token, pool_addrs) in &self.token_index {
            let mut sorted: Vec<Address> = pool_addrs.clone();
            // Sort by estimated liquidity descending so the most meaningful pairs come first
            sorted.sort_by(|a, b| {
                let la = self.pool_liquidity_estimate(a);
                let lb = self.pool_liquidity_estimate(b);
                lb.cmp(&la)
            });
            // Use per-token-tier max_pairs if configured, else global default (H3)
            let token_limit = self.effective_max_pairs(_token);
            let limit = if token_limit == 0 {
                sorted.len()
            } else {
                token_limit.min(sorted.len())
            };
            for i in 0..limit {
                for j in (i + 1)..limit {
                    let a = sorted[i];
                    let b = sorted[j];
                    let key = if a < b { (a, b) } else { (b, a) };
                    if seen.insert(key) {
                        pairs.push((key.0, key.1, *_token));
                    }
                }
            }
        }

        let cached = Arc::new(pairs);
        *self.pairs_cache.lock().expect("pairs_cache mutex poisoned") = Some(Arc::clone(&cached));
        cached
    }

    /// Mark a pool as dirty (state changed). Dirty pools restrict subsequent
    /// incremental detection passes via [`ScanScope::Dirty`].
    pub fn mark_dirty_pool(&mut self, address: Address) {
        self.dirty_pools.insert(address);
    }

    /// Drain the current dirty-pool set, returning it and resetting tracking.
    pub fn take_dirty_pools(&mut self) -> HashSet<Address> {
        std::mem::take(&mut self.dirty_pools)
    }

    /// Number of pools currently marked dirty.
    pub fn dirty_pool_count(&self) -> usize {
        self.dirty_pools.len()
    }

    // ------------------------------------------------------------------
    // #9: runtime transfer-tax learning
    // ------------------------------------------------------------------

    /// Whether `token` is known to be transfer-taxed: listed in the static FOT
    /// registry or dynamically learned from replayed swaps this session.
    pub fn is_taxed_token(&self, token: &Address) -> bool {
        is_fee_on_transfer_token(token) || self.dynamic_fot.contains(token)
    }

    /// Mark a token as transfer-taxed for the remainder of the run.
    pub fn flag_taxed_token(&mut self, token: Address) {
        self.dynamic_fot.insert(token);
    }

    /// Learn taxed tokens from one transaction's logs (#9).
    ///
    /// Correlates each swap event's *declared* amounts with what the involved
    /// ERC20 contracts actually transferred to/from the pool in the same
    /// transaction. A persistent shortfall beyond [`TAX_SHORTFALL_MARGIN`]
    /// means value vanished inside a token transfer (sell/buy tax), so the
    /// token is flagged and excluded by the arb detectors via
    /// [`Self::is_taxed_token`]. Rebase tokens are skipped — their supply
    /// mechanics mimic taxes without being one.
    pub fn learn_taxes_from_tx(&mut self, logs: &[ExecutedLog]) {
        // Pass 1: actual ERC20 movements touching tracked pools, per (pool, token).
        let mut incoming: TransferTotals = HashMap::new();
        let mut outgoing: TransferTotals = HashMap::new();
        for log in logs {
            if log.topics.len() < 3 || log.data.len() < 32 || log.topics[0] != ERC20_TRANSFER_TOPIC
            {
                continue;
            }
            let value = u128_from_be_bytes(&log.data[..32]);
            if value == 0 {
                continue;
            }
            let from = Address::from_word(log.topics[1]);
            let to = Address::from_word(log.topics[2]);
            // The Transfer emitter is the token contract (`log.address`);
            // pools are identified by their role as sender / recipient.
            if self.known_set.contains(&to) {
                *incoming.entry((to, log.address)).or_default() += value;
            }
            if self.known_set.contains(&from) {
                *outgoing.entry((from, log.address)).or_default() += value;
            }
        }
        // No early exit when both maps are empty: a declared swap output with
        // no corresponding transfer at all is itself a total-shortfall signal.

        // Pass 2: gather declared swap amounts (immutable borrows), then flag.
        let mut declarations: Vec<SwapDeclaration> = Vec::new();
        for log in logs {
            if log.topics.is_empty() || !self.pools.contains_key(&log.address) {
                continue;
            }
            let topic0 = log.topics[0];
            if topic0 == SWAP_TOPIC {
                // V2-family Swap (also Solidly/Camelot stable pools): tokens
                // come from pool info, declared amounts from event data.
                let Some(info) = self.pools.get(&log.address).map(|p| p.info().clone()) else {
                    continue;
                };
                if log.data.len() < 128 {
                    continue;
                }
                let amt0_in = u128_from_be_bytes(&log.data[..32]);
                let amt1_in = u128_from_be_bytes(&log.data[32..64]);
                let amt0_out = u128_from_be_bytes(&log.data[64..96]);
                let amt1_out = u128_from_be_bytes(&log.data[96..128]);
                // One row per token: its own input and output declarations.
                declarations.push((
                    log.address,
                    Some(info.token0),
                    amt0_in,
                    Some(info.token0),
                    amt0_out,
                ));
                declarations.push((
                    log.address,
                    Some(info.token1),
                    amt1_in,
                    Some(info.token1),
                    amt1_out,
                ));
                continue;
            }
            if topic0 == decoders::V3_SWAP_TOPIC {
                let Some(d) = decoders::decode_v3_swap(log) else {
                    continue;
                };
                let Some(info) = self.pools.get(&log.address).map(|p| p.info().clone()) else {
                    continue;
                };
                // Positive amount = pool received (input); negative = paid out.
                let (tin, din) = if d.amount0 > 0 {
                    (Some(info.token0), d.amount0.unsigned_abs())
                } else {
                    (Some(info.token1), 0)
                };
                let (tout, dout) = if d.amount1 < 0 {
                    (Some(info.token1), d.amount1.unsigned_abs())
                } else {
                    (Some(info.token0), 0)
                };
                declarations.push((log.address, tin, din, tout, dout));
                continue;
            }
            if topic0 == decoders::CURVE_TOKEN_EXCHANGE_TOPIC
                || topic0 == decoders::CURVE_V2_TOKEN_EXCHANGE_TOPIC
            {
                let Some(d) = decoders::decode_curve_swap(log) else {
                    continue;
                };
                let Some(PoolState::Curve(state)) = self.pools.get(&log.address) else {
                    continue;
                };
                let token_at = |idx: u128| -> Option<Address> {
                    state
                        .token_index
                        .iter()
                        .find(|(_, &i)| i as u128 == idx)
                        .map(|(t, _)| *t)
                };
                declarations.push((
                    log.address,
                    token_at(d.coin_sold),
                    d.amount_sold,
                    token_at(d.coin_bought),
                    d.amount_bought,
                ));
                continue;
            }
            if topic0 == decoders::BALANCER_SWAP_TOPIC {
                let Some(d) = decoders::decode_balancer_swap(log) else {
                    continue;
                };
                declarations.push((
                    log.address,
                    Some(d.token_in),
                    d.amount_in,
                    Some(d.token_out),
                    d.amount_out,
                ));
                continue;
            }
            if topic0 == decoders::LB_SWAP_TOPIC {
                let Some(d) = decoders::decode_lb_swap(log) else {
                    continue;
                };
                declarations.push((
                    log.address,
                    Some(d.token_in),
                    d.amount_in,
                    Some(d.token_out),
                    d.amount_out,
                ));
                continue;
            }
            if topic0 == decoders::PENDLE_SWAP_TOPIC {
                let Some(d) = decoders::decode_pendle_swap(log) else {
                    continue;
                };
                let Some(PoolState::Pendle(state)) = self.pools.get(&log.address) else {
                    continue;
                };
                let (pt, sy) = (state.pt_address, state.sy_address);
                // Net PT out: SY paid in, PT received out (and vice versa).
                let (tin, tout) = if d.is_net_pt_out { (sy, pt) } else { (pt, sy) };
                declarations.push((
                    log.address,
                    Some(tin),
                    d.amount_in,
                    Some(tout),
                    d.amount_out,
                ));
            }
        }

        for (pool, tin, din, tout, dout) in declarations {
            if din >= MIN_TAX_CHECK_AMOUNT {
                if let Some(t) = tin {
                    let actual = incoming.get(&(pool, t)).copied().unwrap_or(0);
                    Self::flag_if_shortfall(&mut self.dynamic_fot, t, actual, din);
                }
            }
            if dout >= MIN_TAX_CHECK_AMOUNT {
                if let Some(t) = tout {
                    let actual = outgoing.get(&(pool, t)).copied().unwrap_or(0);
                    Self::flag_if_shortfall(&mut self.dynamic_fot, t, actual, dout);
                }
            }
        }
    }

    fn flag_if_shortfall(set: &mut HashSet<Address>, token: Address, actual: u128, declared: u128) {
        if is_rebase_token(&token) {
            return; // elastic supply mimics taxes without being one
        }
        let limit = declared as f64 * (1.0 - TAX_SHORTFALL_MARGIN);
        if (actual as f64) < limit {
            set.insert(token);
        }
    }
}

#[cfg(test)]
mod tax_learning_tests {
    use super::*;
    use crate::data::ExecutedLog;
    use alloy::primitives::{address, Bytes};

    const POOL: Address = address!("aa00000000000000000000000000000000000001");
    const T_IN: Address = address!("bb00000000000000000000000000000000000001");
    const T_OUT: Address = address!("bb00000000000000000000000000000000000002");

    fn fixture() -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: crate::pool::state::PoolInfo {
                address: POOL,
                token0: T_IN,
                token1: T_OUT,
                fee: 30,
                dex_type: crate::dex_type::DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        }));
        pm
    }

    fn word(addr: Address) -> B256 {
        addr.into_word()
    }

    fn amount_word(v: u128) -> Bytes {
        let mut b = vec![0u8; 32];
        b[16..].copy_from_slice(&v.to_be_bytes());
        Bytes::from(b)
    }

    fn transfer(token: Address, from: Address, to: Address, value: u128) -> ExecutedLog {
        ExecutedLog {
            address: token,
            topics: vec![ERC20_TRANSFER_TOPIC, word(from), word(to)],
            data: amount_word(value),
        }
    }

    /// V2 Swap on POOL: spends `amt_in` of T_IN (token0), receives `amt_out`
    /// of T_OUT (token1). Data layout: (amt0_in, amt1_in, amt0_out, amt1_out).
    fn swap(amt_in: u128, amt_out: u128) -> ExecutedLog {
        ExecutedLog {
            address: POOL,
            topics: vec![SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: Bytes::from(
                [
                    amount_word(amt_in).to_vec(),  // amt0_in  = T_IN paid in
                    amount_word(0).to_vec(),       // amt1_in
                    amount_word(0).to_vec(),       // amt0_out
                    amount_word(amt_out).to_vec(), // amt1_out = T_OUT paid out
                ]
                .concat(),
            ),
        }
    }

    #[test]
    fn sell_tax_shortfall_flags_token() {
        let mut pm = fixture();
        let logs = vec![
            swap(50_000, 10_000),
            transfer(
                T_IN,
                address!("cc00000000000000000000000000000000000001"),
                POOL,
                50_000,
            ),
            transfer(
                T_OUT,
                POOL,
                address!("dd00000000000000000000000000000000000001"),
                7_500,
            ), // 25% short
        ];
        pm.learn_taxes_from_tx(&logs);
        assert!(pm.is_taxed_token(&T_OUT), "output token must be flagged");
        assert!(
            !pm.is_taxed_token(&T_IN),
            "fully-delivered input must not be flagged"
        );
    }

    #[test]
    fn exact_delivery_flags_nothing() {
        let mut pm = fixture();
        let logs = vec![
            swap(50_000, 10_000),
            transfer(
                T_IN,
                address!("cc00000000000000000000000000000000000001"),
                POOL,
                50_000,
            ),
            transfer(
                T_OUT,
                POOL,
                address!("dd00000000000000000000000000000000000001"),
                10_000,
            ),
        ];
        pm.learn_taxes_from_tx(&logs);
        assert!(!pm.is_taxed_token(&T_OUT));
        assert!(!pm.is_taxed_token(&T_IN));
    }

    #[test]
    fn rebase_tokens_are_never_flagged() {
        let Some(rb) = crate::pool::state::pool_types::test_rebase_token() else {
            return; // empty registry in this build
        };
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: crate::pool::state::PoolInfo {
                address: POOL,
                token0: T_IN,
                token1: rb,
                fee: 30,
                dex_type: crate::dex_type::DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        }));
        let logs = vec![
            ExecutedLog {
                address: POOL,
                topics: vec![SWAP_TOPIC, B256::ZERO, B256::ZERO],
                data: Bytes::from(
                    [
                        amount_word(0).to_vec(),
                        amount_word(0).to_vec(),
                        amount_word(0).to_vec(),
                        amount_word(10_000).to_vec(), // token1 (=rb) declared out
                        amount_word(0).to_vec(),
                    ]
                    .concat(),
                ),
            },
            transfer(
                rb,
                POOL,
                address!("ee00000000000000000000000000000000000001"),
                100,
            ),
        ];
        pm.learn_taxes_from_tx(&logs);
        assert!(
            !pm.dynamic_fot.contains(&rb),
            "rebase tokens must be exempt from dynamic tax flagging"
        );
    }

    #[test]
    fn missing_transfer_counts_as_total_shortfall() {
        let mut pm = fixture();
        // Swap declares output but no Transfer for T_OUT exists at all.
        let logs = vec![swap(50_000, 10_000)];
        pm.learn_taxes_from_tx(&logs);
        assert!(pm.is_taxed_token(&T_OUT));
    }
}

impl PoolManager {
    /// Count pools that have non-zero reserves (i.e., initialized).
    pub fn initialized_count(&self) -> usize {
        self.pools
            .values()
            .filter(|p| match p {
                PoolState::UniswapV2(s) => s.reserve0 > 0 && s.reserve1 > 0,
                PoolState::UniswapV3(s) => s.liquidity > 0,
                PoolState::UniswapV4(s) => s.liquidity > 0,
                PoolState::Curve(s) => s.balances.iter().all(|b| *b > 0),
                PoolState::Balancer(s) => s.balances.iter().all(|b| *b > 0),
                PoolState::TraderJoeLB(s) => s.reserve_x > 0 && s.reserve_y > 0,
                PoolState::Pendle(s) => s.total_pt > 0 && s.total_sy > 0,
            })
            .count()
    }

    /// Check if the given address is the wrapped native token (e.g., WMATIC, WETH).
    pub fn is_wrapped_native(&self, token: &Address) -> bool {
        self.wrapped_native.as_ref() == Some(token)
    }

    /// Get the wrapped native token address (e.g., WMATIC, WETH), if set.
    pub fn wrapped_native(&self) -> Option<Address> {
        self.wrapped_native
    }

    /// Convert an amount from the given token to its wrapped native equivalent,
    /// using a V2/V3/Curve/Balancer pool that pairs the token with wrapped native,
    /// or a 2-hop path through an intermediate token.
    ///
    /// For the direct path: uses the pool's quoting function (V2 spot rate, V3 exact-in,
    /// Curve StableSwap, Balancer weighted product).
    ///
    /// When no direct pool exists, searches for a 2-hop path:
    ///   token -> intermediate -> native
    /// using V2 pools (fast, closed-form). Returns None if no path is found.
    pub fn normalize_to_native(&self, token: Address, amount: u128) -> Option<u128> {
        let native = self.wrapped_native()?;
        if token == native {
            return Some(amount);
        }
        // 1. Try direct pair via unified dispatcher
        if let Some(pool_addr) = self.find_pair_pool(&token, &native) {
            if let Some(pool) = self.get(&pool_addr) {
                let result = crate::pool::math::quote_exact_in(pool, token, native, amount);
                if result.is_some() {
                    return result;
                }
            }
        }

        // 2. C5 fallback: try a 2-hop path through an intermediate token
        //    token -> intermediate -> native
        self.normalize_to_native_multi_hop(token, amount, native)
    }

    /// 2-hop normalization fallback: token -> intermediate -> native.
    /// Iterates through pools trading `token` to find an intermediate token
    /// that itself has a pool pairing with `native`.
    fn normalize_to_native_multi_hop(
        &self,
        token: Address,
        amount: u128,
        native: Address,
    ) -> Option<u128> {
        let token_pools = self.pools_for_token(&token)?;
        // Limit search to avoid excessive iteration
        for &pool_addr in token_pools.iter().take(10) {
            let pool = self.get(&pool_addr)?;
            let intermediate = if pool.info().token0 == token {
                pool.info().token1
            } else {
                pool.info().token0
            };
            if intermediate.is_zero() || intermediate == native {
                continue;
            }
            // Check if intermediate trades with native
            if self.find_pair_pool(&intermediate, &native).is_none() {
                continue;
            }
            // Step 1: token -> intermediate via unified dispatcher
            let mid_amount = match pool {
                PoolState::UniswapV2(v2) => {
                    let (reserve_token, reserve_intermediate) = if v2.info.token0 == token {
                        (v2.reserve0, v2.reserve1)
                    } else {
                        (v2.reserve1, v2.reserve0)
                    };
                    if reserve_token == 0 {
                        continue;
                    }
                    amount
                        .saturating_mul(reserve_intermediate)
                        .saturating_div(reserve_token)
                }
                other => crate::pool::math::quote_exact_in(other, token, intermediate, amount)?,
            };
            if mid_amount == 0 {
                continue;
            }
            // Step 2: intermediate -> native
            let native_amount = self.normalize_to_native(intermediate, mid_amount)?;
            return Some(native_amount);
        }
        None
    }

    /// Get V2 pool state by address (returns None if not a V2 pool or not found).
    pub fn get_v2_state(&self, address: &Address) -> Option<&UniswapV2PoolState> {
        self.pools.get(address).and_then(|p| {
            if let PoolState::UniswapV2(state) = p {
                Some(state)
            } else {
                None
            }
        })
    }

    /// Get V3 pool state by address (returns None if not a V3 pool or not found).
    pub fn get_v3_state(&self, address: &Address) -> Option<&UniswapV3PoolState> {
        self.pools.get(address).and_then(|p| {
            if let PoolState::UniswapV3(state) = p {
                Some(state)
            } else {
                None
            }
        })
    }

    /// Given V3/V4 sqrt price, liquidity, and token ordering, compute
    /// (tvl, tvl * price) as a TVL proxy for on-chain price oracle weighting.
    fn sqrt_price_reserves(
        token0: &Address,
        native: &Address,
        sqrt_price_x96: U256,
        liquidity: u128,
    ) -> Option<(u128, u128)> {
        if liquidity == 0 {
            return None;
        }
        let tvl = liquidity;
        // Use sqrt price for direction: if native is token0,
        // price = (sqrtPriceX96 / 2^96)^2 token1 per token0
        let price = if *token0 == *native {
            let sqrt = sqrt_price_x96;
            if sqrt.is_zero() {
                return None;
            }
            let p_u256: U256 = sqrt.saturating_mul(sqrt) >> 192;
            let p = p_u256.saturating_to::<u128>();
            if p == 0 {
                return None;
            }
            p
        } else {
            let sqrt = sqrt_price_x96;
            if sqrt.is_zero() {
                return None;
            }
            let one: U256 = U256::from(1u128) << 192;
            let inv: U256 = one / sqrt;
            let p_u256: U256 = inv.saturating_mul(inv) >> 192;
            let p = p_u256.saturating_to::<u128>();
            if p == 0 {
                return None;
            }
            p
        };
        Some((tvl, tvl.saturating_mul(price)))
    }

    /// Derive native token USD price from the highest-TVL pool that pairs
    /// wrapped native with a stablecoin (USDC, USDT, DAI).
    /// Returns `None` if no suitable pool is found.
    ///
    /// Price = reserve_stable / reserve_native, adjusted for token decimals.
    /// Used as an on-chain oracle fallback (L5).
    pub fn onchain_native_price(&self, stable_tokens: &[Address]) -> Option<f64> {
        let native = self.wrapped_native()?;
        let mut best_price: Option<f64> = None;
        let mut best_tvl: u128 = 0;
        for &stable in stable_tokens {
            let pool_addr = self.find_pair_pool(&native, &stable)?;
            let pool = self.get(&pool_addr)?;
            let (reserve_native, reserve_stable) = match pool {
                PoolState::UniswapV2(v2) => {
                    if v2.info.token0 == native {
                        (v2.reserve0, v2.reserve1)
                    } else {
                        (v2.reserve1, v2.reserve0)
                    }
                }
                PoolState::UniswapV3(v3) => {
                    match Self::sqrt_price_reserves(
                        &v3.info.token0,
                        &native,
                        v3.sqrt_price_x96,
                        v3.liquidity,
                    ) {
                        Some(r) => r,
                        None => continue,
                    }
                }
                PoolState::UniswapV4(v4) => {
                    match Self::sqrt_price_reserves(
                        &v4.info.token0,
                        &native,
                        v4.sqrt_price_x96,
                        v4.liquidity,
                    ) {
                        Some(r) => r,
                        None => continue,
                    }
                }
                PoolState::Curve(curve) => {
                    let idx_native = curve.token_index.get(&native)?;
                    let idx_stable = curve.token_index.get(&stable)?;
                    let bal_native = curve.balances.get(*idx_native)?;
                    let bal_stable = curve.balances.get(*idx_stable)?;
                    (*bal_native, *bal_stable)
                }
                PoolState::Balancer(bal) => {
                    let idx_native = bal.token_index.get(&native)?;
                    let idx_stable = bal.token_index.get(&stable)?;
                    let bal_native = bal.balances.get(*idx_native)?;
                    let bal_stable = bal.balances.get(*idx_stable)?;
                    (*bal_native, *bal_stable)
                }
                PoolState::TraderJoeLB(lb) => {
                    if lb.info.token0 == native {
                        (lb.reserve_x, lb.reserve_y)
                    } else {
                        (lb.reserve_y, lb.reserve_x)
                    }
                }
                PoolState::Pendle(p) => {
                    if p.info.token0 == native {
                        (p.total_pt, p.total_sy)
                    } else {
                        (p.total_sy, p.total_pt)
                    }
                }
            };
            let tvl = reserve_native.saturating_mul(reserve_stable).max(1);
            if tvl > best_tvl {
                best_tvl = tvl;
                if reserve_native > 0 && reserve_stable > 0 {
                    best_price = Some(reserve_stable as f64 / reserve_native as f64);
                }
            }
        }
        best_price
    }

    /// Set the wrapped native token address.
    pub fn with_wrapped_native(mut self, addr: Address) -> Self {
        self.wrapped_native = Some(addr);
        self
    }

    /// Set the Balancer V2 vault address for flash loans and pool state queries.
    pub fn with_balancer_vault(mut self, addr: Address) -> Self {
        self.balancer_vault = Some(addr);
        self
    }
}

impl Clone for PoolManager {
    fn clone(&self) -> Self {
        let cache = self
            .pairs_cache
            .lock()
            .expect("pairs_cache mutex poisoned")
            .clone();
        PoolManager {
            pools: self.pools.clone(),
            token_index: self.token_index.clone(),
            pairs_cache: Mutex::new(cache),
            dirty_pools: self.dirty_pools.clone(),
            dynamic_fot: self.dynamic_fot.clone(),
            wrapped_native: self.wrapped_native,
            balancer_vault: self.balancer_vault,
            known_set: self.known_set.clone(),
            max_pairs_per_token: self.max_pairs_per_token,
            token_max_pairs: self.token_max_pairs.clone(),
            concurrency_limit: self.concurrency_limit,
            use_latest: self.use_latest,
        }
    }
}

impl Default for PoolManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a dedup entry has changed sufficiently to re-emit an opportunity.
/// Returns true if the entry is new or pool reserves changed by >0.1%.
pub fn check_dedup_key(
    seen: &mut HashMap<(Address, Address, Address, Address), (u128, u128)>,
    key: &(Address, Address, Address, Address),
    pm: &PoolManager,
    pool_a: Address,
    pool_b: Address,
) -> bool {
    let la = pm.pool_liquidity_estimate(&pool_a);
    let lb = pm.pool_liquidity_estimate(&pool_b);
    let new_snapshot = (la, lb);

    if let Some(&(prev_la, prev_lb)) = seen.get(key) {
        let threshold_a = cmp::max(prev_la / LIQUIDITY_CHANGE_THRESHOLD_DIVISOR, 1);
        let threshold_b = cmp::max(prev_lb / LIQUIDITY_CHANGE_THRESHOLD_DIVISOR, 1);
        if la.abs_diff(prev_la) <= threshold_a && lb.abs_diff(prev_lb) <= threshold_b {
            return false;
        }
    }

    seen.insert(*key, new_snapshot);
    true
}
