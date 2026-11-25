//! Pool discovery — scans chain event logs to find and register new DEX pools.
//!
//! The unified `discover_pools` function scans both DEX activity events
//! (Swap, Sync, Mint, Burn, TokenExchange, BalancerSwap) for active pools
//! AND factory creation events (PairCreated, PoolCreated, PoolRegistered,
//! PoolAdded) for pools created in the range. This ensures comprehensive
//! coverage for backtesting recent periods.
//!
//! **Boundary:** DEX *router* aggregators (1inch, 0x, ParaSwap/Velora, Kyber,
//! OpenOcean, …) are **not** discovery venues — they have no pool state. Their events are
//! decoded only as explorer swap edges. Do not add them as
//! `DexType` / factory scanners. Remote "aggregators" in `remote/` means
//! GeckoTerminal / DexScreener pool lists (TVL sources), not those routers.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use futures::{stream, StreamExt};
use alloy::rpc::types::Filter;
use serde::{Deserialize, Serialize};

use crate::cache::SqliteStore;
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::pool::selectors::SYMBOL;
use crate::pool::state::pool_types::{is_fee_on_transfer_token, is_rebase_token};
use crate::pool::state::PoolInfo;
use crate::rpc::RpcClient;

/// One DEX activity hit keyed by pool address — shared across per-DEX scanners.
#[derive(Clone, Copy)]
pub(crate) struct PoolHit {
    pub(crate) dex_type: DexType,
    pub(crate) pool_id: Option<[u8; 32]>,
    /// Token pair recovered directly from the event (None → fetch via RPC).
    pub(crate) tokens: Option<(Address, Address)>,
    /// Lowest block the pool was seen at.
    pub(crate) first_seen_block: u64,
}

/// DEX activity event hits keyed by pool address — shared across per-DEX scanners.
pub(crate) type PoolHits = HashMap<Address, PoolHit>;

/// Parsed DEX event classification: factory, pool id, token pair, and the
/// override address used when the emitting contract is not the pool itself.
#[derive(Clone, Copy)]
pub(super) struct PoolHitCandidate {
    pub(super) dex_type: DexType,
    pub(super) pool_id: Option<[u8; 32]>,
    pub(super) tokens: Option<(Address, Address)>,
    pub(super) addr_override: Option<Address>,
}

impl PoolHitCandidate {
    /// A hit that carries no event payload — only the DEX type.
    pub(super) fn simple(dex_type: DexType) -> Self {
        PoolHitCandidate {
            dex_type,
            pool_id: None,
            tokens: None,
            addr_override: None,
        }
    }
}

pub(crate) static V2_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PairCreated(address,address,address,uint256)"));

pub(crate) static V3_POOL_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolCreated(address,address,uint24,int24,address)"));

pub(crate) static BALANCER_POOL_REGISTERED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolRegistered(bytes32,address,uint8)"));

pub(crate) static CURVE_POOL_ADDED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolAdded(address,uint256)"));

// CurveStableswapFactoryNG pool-creation event — `PoolDeployed(address pool)`.
pub(crate) static CURVE_POOL_DEPLOYED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolDeployed(address)"));

// Solidly-style PairCreated (bool stable, address pair) — Velodrome, Aerodrome, Equalizer, Thena
pub(crate) static SOLIDLY_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PairCreated(address,address,bool,address)"));

// Camelot PairCreated (address pair, uint256 fee, bool stable)
pub(crate) static CAMELOT_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PairCreated(address,address,address,uint256,bool)"));

// Uniswap V4 Initialize from the singleton PoolManager. Non-indexed data is
// five ABI words: fee, tickSpacing, hooks, sqrtPriceX96, tick.
pub(crate) static V4_INITIALIZE_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)")
});

// Pancake Infinity CL Initialize event from singleton CLPoolManager:
// `Initialize(bytes32 indexed id,address indexed currency0,address indexed currency1,address hooks,uint24 fee,bytes32 parameters,uint160 sqrtPriceX96,int24 tick)`
pub(crate) static INF_CL_INITIALIZE_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"Initialize(bytes32,address,address,address,uint24,bytes32,uint160,int24)")
});

// Trader Joe V2 LB PairCreated event
pub(crate) static LB_PAIR_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"LBPairCreated(address,address,address,uint256,address[])"));

// Pendle Finance NewMarket event
pub(crate) static PENDLE_NEW_MARKET_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"NewMarket(address,address,uint256)"));

// Algebra (QuickSwap V3) Pool creation event — `Pool(address,address,address)` with two indexed tokens.
// Unlike canonical Uniswap V3 `PoolCreated(address,address,uint24,int24,address)`.
pub(crate) static ALGEBRA_POOL_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"Pool(address,address,address)"));

// Aerodrome Slipstream / Velodrome V3 CL pool creation event —
// `PoolCreated(address,address,int24,address)` (token0, token1, tickSpacing indexed; pool in data).
// Bespoke topic, not canonical Uniswap V3 nor Algebra.
pub(crate) static SLIPSTREAM_POOL_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolCreated(address,address,int24,address)"));

// Metric V2 pool creation — `PoolCreated(address indexed token0,
// address indexed token1, address indexed priceProvider, address pool,
// bytes32 poolId)` (digest computed from the signature string; verifying the
// produced topic on-chain is deferred).
pub(crate) static METRIC_POOL_CREATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"PoolCreated(address,address,address,address,bytes32)"));

// Fluid DEX pool deployment — `LogDexDeployed(address indexed dex, uint256
// indexed dexId)` (verified against Instadapp/fluid-contracts-public
// factory/main.sol). Token metadata is fetched later via token0/token1.
pub(crate) static FLUID_DEX_DEPLOYED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"LogDexDeployed(address,uint256)"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: Option<i32>,
    pub dex_type: DexType,
    #[serde(default)]
    pub creation_block: u64,
    /// Balancer V2 pool ID (bytes32), used to query vault for token balances.
    #[serde(default)]
    pub pool_id: Option<[u8; 32]>,
    /// Factory address that created this pool (fork-aware V2 storage slots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<Address>,
    /// Whether the pool is a stable-swap pool (Solidly/Camelot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_stable: Option<bool>,
    /// Balancer pool type from PoolRegistered event (0=Weighted, 1=Weighted2Tokens, 3=ComposableStable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_pool_type: Option<u8>,
    /// Uniswap V4 hook contract address (derived from pool key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_address: Option<Address>,
    /// Trader Joe LB bin step in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_step: Option<u32>,
    /// Pendle Finance market maturity timestamp (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maturity_timestamp: Option<u64>,
    /// Full token list for multi-token pools (Curve 3+, Balancer 2-8).
    /// Populated during discovery for Curve/Balancer pools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying_tokens: Option<Vec<Address>>,
    /// Human-readable DEX protocol name (e.g. "QuickSwap", "SushiSwap", "Velodrome").
    /// Populated from factory address lookup or falls back to DexType label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dex_name: Option<String>,
    /// Token0 symbol (e.g. "WETH", "USDC").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token0_symbol: Option<String>,
    /// Token1 symbol (e.g. "WETH", "USDC").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token1_symbol: Option<String>,
    /// USD TVL from remote sources (subgraph / aggregator). `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvl_usd: Option<f64>,
    /// Rolling 24h USD volume from remote sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_usd_24h: Option<f64>,
    /// Rolling 30d USD volume from remote sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_usd_30d: Option<f64>,
}

macro_rules! merge_option {
    ($self:expr, $other:expr, $field:ident) => {
        if $self.$field.is_none() {
            $self.$field = $other.$field.clone();
        }
    };
}

impl DiscoveredPool {
    pub fn new(
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        dex_type: DexType,
        creation_block: u64,
    ) -> Self {
        Self {
            address,
            token0,
            token1,
            fee,
            dex_type,
            creation_block,
            tick_spacing: None,
            pool_id: None,
            factory: None,
            is_stable: None,
            balancer_pool_type: None,
            hook_address: None,
            bin_step: None,
            maturity_timestamp: None,
            underlying_tokens: None,
            dex_name: None,
            token0_symbol: None,
            token1_symbol: None,
            tvl_usd: None,
            volume_usd_24h: None,
            volume_usd_30d: None,
        }
    }

    pub fn with_tick_spacing(mut self, v: Option<i32>) -> Self {
        self.tick_spacing = v;
        self
    }
    pub fn with_pool_id(mut self, v: Option<[u8; 32]>) -> Self {
        self.pool_id = v;
        self
    }
    pub fn with_factory(mut self, v: Option<Address>) -> Self {
        self.factory = v;
        self
    }
    pub fn with_is_stable(mut self, v: Option<bool>) -> Self {
        self.is_stable = v;
        self
    }
    pub fn with_balancer_pool_type(mut self, v: Option<u8>) -> Self {
        self.balancer_pool_type = v;
        self
    }
    pub fn with_hook_address(mut self, v: Option<Address>) -> Self {
        self.hook_address = v;
        self
    }
    pub fn with_bin_step(mut self, v: Option<u32>) -> Self {
        self.bin_step = v;
        self
    }
    pub fn with_maturity_timestamp(mut self, v: Option<u64>) -> Self {
        self.maturity_timestamp = v;
        self
    }
    pub fn with_underlying_tokens(mut self, v: Option<Vec<Address>>) -> Self {
        self.underlying_tokens = v;
        self
    }
    pub fn with_dex_name(mut self, v: Option<String>) -> Self {
        self.dex_name = v;
        self
    }
    pub fn with_token0_symbol(mut self, v: Option<String>) -> Self {
        self.token0_symbol = v;
        self
    }
    pub fn with_token1_symbol(mut self, v: Option<String>) -> Self {
        self.token1_symbol = v;
        self
    }
    pub fn with_tvl_usd(mut self, v: Option<f64>) -> Self {
        self.tvl_usd = v;
        self
    }
    pub fn with_volume_usd_24h(mut self, v: Option<f64>) -> Self {
        self.volume_usd_24h = v;
        self
    }
    pub fn with_volume_usd_30d(mut self, v: Option<f64>) -> Self {
        self.volume_usd_30d = v;
        self
    }

    /// Merge metadata from `other` into `self`, filling only `None` fields.
    ///
    /// Used to merge partially-complete discovery results from different runs.
    pub fn merge_from(&mut self, other: &DiscoveredPool) {
        debug_assert_eq!(
            self.address, other.address,
            "merge_from called with different addresses"
        );

        // dex_type reconciliation: `UniswapV2` doubles as the remote-source
        // fallback label (`infer_dex_type`), so it must not survive a conflict
        // with a more specific classification, and a specific on-chain-derived
        // type must never be overwritten by a remote guess. A V2 label on a
        // pool with no fee set is treated as the fallback (on-chain V2 pools
        // always carry their explicit default fee); a V2 label with a fee is
        // a confirmed on-chain classification and wins.
        if other.dex_type != self.dex_type {
            match (self.dex_type, other.dex_type) {
                // `Unknown` is "no family inferred", never a classification:
                // any specific type wins over it.
                (DexType::Unknown, specific) => {
                    self.dex_type = specific;
                }
                (_, DexType::Unknown) => {}
                (DexType::UniswapV2, specific) if self.fee == 0 => {
                    self.dex_type = specific;
                }
                (_, DexType::UniswapV2) => {
                    // Keep the existing (specific or confirmed) type.
                }
                (kept, conflicting) => {
                    tracing::debug!(
                        "merge_from: conflicting dex_type for {}: keeping {kept} over {conflicting}",
                        self.address
                    );
                }
            }
        }
        if self.fee == 0 && other.fee != 0 {
            self.fee = other.fee;
        }
        merge_option!(self, other, tick_spacing);
        if self.creation_block == 0 && other.creation_block != 0 {
            self.creation_block = other.creation_block;
        }
        merge_option!(self, other, pool_id);
        merge_option!(self, other, factory);
        merge_option!(self, other, is_stable);
        merge_option!(self, other, balancer_pool_type);
        merge_option!(self, other, hook_address);
        merge_option!(self, other, bin_step);
        merge_option!(self, other, maturity_timestamp);
        merge_option!(self, other, underlying_tokens);
        merge_option!(self, other, dex_name);
        // Symbols are positional (token0/token1): only merge when `other` refers
        // to the same token addresses. Remote aggregators may use base/quote
        // ordering that differs from the canonical sorted order.
        if self.token0_symbol.is_none() && other.token0 == self.token0 && !self.token0.is_zero() {
            self.token0_symbol = other.token0_symbol.clone();
        }
        if self.token1_symbol.is_none() && other.token1 == self.token1 && !self.token1.is_zero() {
            self.token1_symbol = other.token1_symbol.clone();
        }
        merge_option!(self, other, tvl_usd);
        merge_option!(self, other, volume_usd_24h);
        merge_option!(self, other, volume_usd_30d);
    }
}

impl From<DiscoveredPool> for PoolInfo {
    fn from(d: DiscoveredPool) -> Self {
        let is_fot =
            Some(is_fee_on_transfer_token(&d.token0) || is_fee_on_transfer_token(&d.token1));
        let is_rebase = Some(is_rebase_token(&d.token0) || is_rebase_token(&d.token1));
        PoolInfo {
            address: d.address,
            token0: d.token0,
            token1: d.token1,
            fee: d.fee,
            name: None,
            dex_type: d.dex_type,
            tick_spacing: d.tick_spacing.map(|ts| ts as u32),
            creation_block: d.creation_block,
            pool_id: d.pool_id,
            factory: d.factory,
            is_stable: d.is_stable,
            is_fot,
            is_rebase,
            underlying_tokens: d.underlying_tokens,
            balancer_pool_type: d.balancer_pool_type,
            hook_address: d.hook_address,
            bin_step: d.bin_step,
            maturity_timestamp: d.maturity_timestamp,
            dex_name: d.dex_name.map(Arc::from),
            derivation_version: Some(crate::dex_type::POOL_DERIVATION_VERSION),
            token0_symbol: d.token0_symbol.map(Arc::from),
            token1_symbol: d.token1_symbol.map(Arc::from),
            tvl_usd: d.tvl_usd,
            volume_usd_24h: d.volume_usd_24h,
            volume_usd_30d: d.volume_usd_30d,
        }
    }
}

/// Process a single DEX activity log, updating active_blocks and pool_hits.
fn process_discovery_log(
    log: &alloy::rpc::types::Log,
    active_blocks: &mut HashSet<u64>,
    pool_hits: &mut PoolHits,
) {
    if let Some(bn) = log.block_number {
        active_blocks.insert(bn);
    }
    if let Some(candidate) = classify_dex_event(log) {
        // Singleton architectures (e.g. Balancer Vault) emit from a router
        // contract; the real pool address is recovered from event data/topics.
        let addr = candidate.addr_override.unwrap_or_else(|| log.address());
        let block_num = log.block_number.unwrap_or(0);
        let entry = pool_hits.entry(addr).or_insert(PoolHit {
            dex_type: candidate.dex_type,
            pool_id: candidate.pool_id,
            tokens: candidate.tokens,
            first_seen_block: block_num,
        });
        if block_num > 0 && block_num < entry.first_seen_block {
            entry.first_seen_block = block_num;
        }
    }
}

/// Configuration for pool discovery parameters.
pub struct DiscoveryConfig<'a> {
    pub batch_size: u64,
    /// Factory-creation `eth_getLogs` window. At least `batch_size`.
    pub factory_batch_size: u64,
    pub v2_fee_override: Option<u32>,
    pub balancer_vault: Option<Address>,
    pub v2_factories: Option<&'a [Address]>,
    pub v3_factories: Option<&'a [Address]>,
    pub curve_registry: Option<Address>,
    /// Curve stableswap factory addresses (`PoolAdded`/`PoolDeployed` are both scanned).
    pub curve_factories: Option<&'a [Address]>,
    pub solidly_factories: Option<&'a [Address]>,
    pub camelot_factories: Option<&'a [Address]>,
    /// Uniswap V4 singleton PoolManager contract address.
    pub v4_pool_manager: Option<Address>,
    /// Pancake Infinity singleton CLPoolManager contract address (BSC).
    pub infinity_cl_pool_manager: Option<Address>,
    /// Metric V2 AMM factory address (single deterministic factory per chain).
    pub metric_factory: Option<Address>,
    /// Fluid DEX factory address (same address on all chains).
    pub fluid_factory: Option<Address>,
    /// Solidly-style pool fee in basis points (default: 30).
    pub solidly_fee_bps: Option<u32>,
    /// Trader Joe / LFJ V2 LB factory contract addresses (V2.1 + V2.2 can coexist).
    pub trader_joe_factories: Option<&'a [Address]>,
    /// Pendle Finance factory contract address.
    pub pendle_factory: Option<Address>,
    /// Max concurrent RPC calls for metadata fetch (default: 64).
    pub rpc_concurrency: usize,
    /// Token symbol cache — avoids redundant `symbol` eth_call RPC calls.
    pub token_cache: Option<&'a crate::cache::TokenCache>,
    /// Pool metadata cache — skips token0/token1/fee eth_call for already-known pools.
    pub pool_cache: Option<&'a crate::cache::SqliteStore>,
}

/// Resolve factory addresses for a DEX kind: use the chain-config overrides
/// when present, otherwise fall back to the bundled per-chain defaults.
/// Unparseable default entries are silently dropped (they are compile-time
/// constants; a parse failure is a data bug we don't want to crash discovery).
///
/// Shared by the `discover` CLI command, which builds
/// their `DiscoveryConfig`s through this same selection.
pub fn pick_factories(configured: Vec<Address>, defaults: &[&str]) -> Vec<Address> {
    if !configured.is_empty() {
        return configured;
    }
    defaults.iter().filter_map(|s| s.parse().ok()).collect()
}

#[cfg(test)]
mod factory_selection_tests {
    use super::{pick_factories, Address};

    #[test]
    fn prefers_configured_over_defaults() {
        let configured = vec![Address::ZERO];
        let defaults = ["0x4444444444444444444444444444444444444444"];
        assert_eq!(pick_factories(configured.clone(), &defaults), configured);
    }

    #[test]
    fn falls_back_to_parsed_defaults() {
        let defaults = ["0x4444444444444444444444444444444444444444"];
        let got = pick_factories(Vec::new(), &defaults);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn drops_unparseable_defaults() {
        assert!(pick_factories(Vec::new(), &["nothex"]).is_empty());
    }
}

// ── Helper: decode ABI-encoded string from eth_call response ──
fn decode_abi_string(data: &[u8]) -> Option<String> {
    if data.len() < 32 {
        return None;
    }
    // ABI-encoded string: first 32 bytes = offset (typically 0x20)
    let offset = U256::from_be_slice(&data[..32]).to::<usize>();
    if offset + 32 > data.len() {
        return None;
    }
    // At offset: length of the string
    let len = U256::from_be_slice(&data[offset..offset + 32]).to::<usize>();
    let start = offset + 32;
    let end = start + len;
    if end > data.len() {
        return None;
    }
    let bytes = &data[start..end];
    // Trim trailing null padding and non-printable chars
    let trimmed = bytes
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| &bytes[..=i])
        .unwrap_or(bytes);
    String::from_utf8(trimmed.to_vec()).ok()
}

// ── Helper: retry wrapper for eth_getLogs with adaptive range splitting ──

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1_000;
const MIN_SPLIT_BATCH: u64 = 10;

/// Probe the RPC's `eth_getLogs` block range limit by binary-searching with
/// increasingly larger empty filters. Returns the largest range that succeeds,
/// capped at `requested`.
async fn probe_get_logs_limit(rpc: &RpcClient, to_block: u64, requested: u64) -> u64 {
    let candidates: &[u64] = &[100, 250, 500, 1000, 2000, 5000, 10_000];
    let mut working = MIN_SPLIT_BATCH;
    // An unlimited node is probed alone so a public peer's smaller cap is not
    // adopted. All-positive RPS sets keep the multi-provider `get_logs` path.
    let pin = rpc.unlimited_provider_index().await;
    for &candidate in candidates {
        if candidate > requested {
            break;
        }
        let from = to_block.saturating_sub(candidate);
        let filter = Filter::new().from_block(from).to_block(to_block);
        let call = async {
            match pin {
                Some(idx) => rpc.get_logs_pinned_strict(idx, &filter).await,
                None => rpc.get_logs(&filter).await,
            }
        };

        match tokio::time::timeout(Duration::from_secs(15), call).await {
            Ok(Ok(_)) => {
                working = candidate;
            }
            Ok(Err(e)) if is_block_range_error(&e) => {
                tracing::info!("RPC eth_getLogs limit detected at ~{candidate} blocks");
                break;
            }
            Ok(Err(_)) | Err(_) => {
                continue;
            }
        }
    }
    working.min(requested)
}

fn is_block_range_error(e: &anyhow::Error) -> bool {
    let msg = format!("{e:#}");
    msg.contains("Block range")
        || msg.contains("block range")
        || msg.contains("-32062")
        || msg.contains("-32001")
        || msg.contains("range too large")
        || msg.contains("too many results")
}

async fn get_logs_with_retry(
    rpc: &RpcClient,
    filter: &Filter,
    batch_start: u64,
    batch_end: u64,
) -> anyhow::Result<Vec<alloy::rpc::types::Log>> {
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match rpc.get_logs(filter).await {
            Ok(logs) => return Ok(logs),
            Err(e) => {
                if is_block_range_error(&e) && (batch_end - batch_start) >= MIN_SPLIT_BATCH {
                    tracing::warn!(
                        "Block range too large for {batch_start}..{batch_end}: {e:#}. \
                         Splitting into smaller batches..."
                    );
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return get_logs_split_recursive(rpc, filter, batch_start, batch_end).await;
                }
                if attempt < MAX_RETRIES - 1 {
                    let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                    tracing::warn!(
                        "get_logs failed for {batch_start}..{batch_end} (attempt {}/{}): {:#}. \
                         Retrying in {}ms...",
                        attempt + 1,
                        MAX_RETRIES,
                        e,
                        delay
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                last_err = Some(e);
            }
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Err(anyhow::anyhow!(
            "get_logs retry loop for {batch_start}..{batch_end} exited after \
             {MAX_RETRIES} attempts without recording an error"
        )),
    }
}

async fn get_logs_split_recursive(
    rpc: &RpcClient,
    base_filter: &Filter,
    from: u64,
    to: u64,
) -> anyhow::Result<Vec<alloy::rpc::types::Log>> {
    if from > to {
        return Ok(Vec::new());
    }
    let range = to - from + 1;
    if range <= MIN_SPLIT_BATCH {
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            let f = base_filter.clone().from_block(from).to_block(to);
            match rpc.get_logs(&f).await {
                Ok(logs) => return Ok(logs),
                Err(e) => {
                    if is_block_range_error(&e) && range > 1 {
                        tracing::warn!(
                            "Block range still too large for {from}..{to} ({range} blocks): {e:#}. Halving..."
                        );
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        let mid = from + range / 2 - 1;
                        let left_f = base_filter.clone().from_block(from).to_block(mid);
                        let right_f = base_filter.clone().from_block(mid + 1).to_block(to);
                        let mut results =
                            Box::pin(get_logs_split_recursive(rpc, &left_f, from, mid)).await?;
                        results.extend(
                            Box::pin(get_logs_split_recursive(rpc, &right_f, mid + 1, to)).await?,
                        );
                        return Ok(results);
                    }
                    tracing::warn!(
                        "Split get_logs failed for {from}..{to} (attempt {}/{}): {e:#}.",
                        attempt + 1,
                        MAX_RETRIES
                    );
                }
            }
        }
        tracing::warn!("Skipping range {from}..{to} after {} retries", MAX_RETRIES);
        return Ok(Vec::new());
    }
    let mid = from + range / 2 - 1;
    let left_f = base_filter.clone().from_block(from).to_block(mid);
    let right_f = base_filter.clone().from_block(mid + 1).to_block(to);
    let left = Box::pin(get_logs_split_recursive(rpc, &left_f, from, mid)).await?;
    let right = Box::pin(get_logs_split_recursive(rpc, &right_f, mid + 1, to)).await?;
    let mut combined = left;
    combined.extend(right);
    Ok(combined)
}

// ── Helper: classify a DEX event log ──

/// Returns `(dex_type, pool_id, tokens, addr_override)` for a recognized DEX
/// event log, delegating to the self-contained per-DEX modules.
///
/// `addr_override` is set when the emitting contract is not the pool itself
/// (e.g. the Balancer Vault singleton): the pool address is derived from the
/// event (Balancer poolIds encode the pool address in their top 20 bytes).
fn classify_dex_event(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    v2::classify_activity(log)
        .or_else(|| v3::classify_activity(log))
        .or_else(|| balancer::classify_activity(log))
        .or_else(|| curve::classify_activity(log))
        .or_else(|| trader_joe::classify_activity(log))
        .or_else(|| pendle::classify_activity(log))
        .or_else(|| v4::classify_activity(log))
        .or_else(|| infinity::classify_activity(log))
        .or_else(|| fluid::classify_activity(log))
        .or_else(|| metric::classify_activity(log))
}

// ── Helper: scan factory creation events ──

/// Per-DEX batch-scan result, merged by the caller.
///
/// Factory/activity scanners only ever insert into these three collections, so
/// returning them lets the per-DEX scans run concurrently (`join_all`) and merge
/// afterwards instead of serializing every batch on the slowest provider.
#[derive(Default)]
pub(super) struct ScanBatchResult {
    pub(super) active_blocks: HashSet<u64>,
    pub(super) pool_hits: PoolHits,
    pub(super) factory_pools: HashMap<Address, DiscoveredPool>,
}

impl ScanBatchResult {
    /// Fold another batch result into this one.
    ///
    /// `or_insert` keeps the first discovery of an address — the same
    /// first-wins semantics as the old shared mutable maps.
    pub(super) fn merge(&mut self, other: Self) {
        self.active_blocks.extend(other.active_blocks);
        for (addr, hit) in other.pool_hits {
            self.pool_hits.entry(addr).or_insert(hit);
        }
        for (addr, pool) in other.factory_pools {
            self.factory_pools.entry(addr).or_insert(pool);
        }
    }
}

/// Shared inputs for one DEX's factory batch scan: the RPC/config plumbing
/// every scanner needs plus the block range being scanned and the provider
/// pin for this shard. Keeps `scan_*_batch` signatures to a single argument.
#[derive(Clone, Copy)]
pub(super) struct ScanContext<'a> {
    pub(super) rpc: &'a RpcClient,
    pub(super) config: &'a DiscoveryConfig<'a>,
    pub(super) current: u64,
    pub(super) batch_end: u64,
    pub(super) provider_idx: Option<usize>,
}

/// Pins to a specific provider when `provider_idx` is `Some`.
async fn scan_factory_creation_events_pinned(
    rpc: &RpcClient,
    factories: &[Address],
    topic: B256,
    from_block: u64,
    to_block: u64,
    provider_idx: Option<usize>,
    decode_fn: impl Fn(&alloy::rpc::types::Log) -> Option<(Address, DiscoveredPool)>,
) -> ScanBatchResult {
    let mut out = ScanBatchResult::default();
    if factories.is_empty() {
        return out;
    }
    let filter = Filter::new()
        .address(factories.to_vec())
        .event_signature(topic)
        .from_block(from_block)
        .to_block(to_block);
    let result = match provider_idx {
        Some(idx) => rpc.get_logs_for(idx, &filter).await,
        None => get_logs_with_retry(rpc, &filter, from_block, to_block).await,
    };
    match result {
        Ok(logs) => {
            for log in &logs {
                if let Some(bn) = log.block_number {
                    out.active_blocks.insert(bn);
                }
                if let Some((addr, pool)) = decode_fn(log) {
                    out.factory_pools.entry(addr).or_insert(pool);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Factory scan failed for {from_block}..{to_block} (topic {topic}): {e:#}"
            );
        }
    }
    out
}

/// Decode a factory-creation log that came from a receipt scan.
///
/// The `getLogs` scanners filter by address in the RPC request; receipt scans
/// see *every* log in the block, so the same address gating is applied locally
/// here. Keeping the gating in one place means a log decoded from receipts is
/// identical to one decoded from `getLogs`.
pub(super) fn decode_factory_creation_log(
    log: &alloy::rpc::types::Log,
    config: &DiscoveryConfig<'_>,
) -> Option<(Address, DiscoveredPool)> {
    let topics = log.topics();
    let topic0 = *topics.first()?;
    let emitter = log.address();
    let is_authority = |addrs: Option<&[Address]>| addrs.is_some_and(|a| a.contains(&emitter));

    if topic0 == *V2_PAIR_CREATED_TOPIC && is_authority(config.v2_factories) {
        return v2::decode_v2_pair_created(log, config.v2_fee_override.unwrap_or(30));
    }
    if topic0 == *V3_POOL_CREATED_TOPIC && is_authority(config.v3_factories) {
        return v3::decode_v3_pool_created(log);
    }
    if topic0 == *ALGEBRA_POOL_CREATED_TOPIC && is_authority(config.v3_factories) {
        return v3::decode_algebra_pool_created(log);
    }
    if topic0 == *SLIPSTREAM_POOL_CREATED_TOPIC && is_authority(config.v3_factories) {
        return v3::decode_slipstream_pool_created(log);
    }
    if topic0 == *BALANCER_POOL_REGISTERED_TOPIC && config.balancer_vault == Some(emitter) {
        return balancer::decode_balancer_pool_registered(log, emitter);
    }
    if topic0 == *CURVE_POOL_ADDED_TOPIC
        && (is_authority(config.curve_factories) || config.curve_registry == Some(emitter))
    {
        return curve::decode_curve_pool_added(log);
    }
    if topic0 == *CURVE_POOL_DEPLOYED_TOPIC
        && (is_authority(config.curve_factories) || config.curve_registry == Some(emitter))
    {
        return curve::decode_curve_pool_deployed(log);
    }
    if topic0 == *SOLIDLY_PAIR_CREATED_TOPIC && is_authority(config.solidly_factories) {
        let fee = config
            .solidly_fee_bps
            .or(config.v2_fee_override)
            .unwrap_or(30);
        return solidly::decode_solidly_pair_created(log, fee);
    }
    if topic0 == *CAMELOT_PAIR_CREATED_TOPIC && is_authority(config.camelot_factories) {
        return camelot::decode_camelot_pair_created(log);
    }
    if topic0 == *LB_PAIR_CREATED_TOPIC && is_authority(config.trader_joe_factories) {
        return trader_joe::decode_lb_pair_created(log, emitter);
    }
    if topic0 == *PENDLE_NEW_MARKET_TOPIC && config.pendle_factory == Some(emitter) {
        return pendle::decode_pendle_new_market(log, emitter);
    }
    if topic0 == *V4_INITIALIZE_TOPIC && config.v4_pool_manager == Some(emitter) {
        return v4::decode_v4_initialize(log, emitter);
    }
    if topic0 == *INF_CL_INITIALIZE_TOPIC && config.infinity_cl_pool_manager == Some(emitter) {
        return infinity::decode_infinity_initialize(log, emitter);
    }
    if topic0 == *METRIC_POOL_CREATED_TOPIC && config.metric_factory == Some(emitter) {
        return metric::decode_metric_pool_created(log);
    }
    if topic0 == *FLUID_DEX_DEPLOYED_TOPIC && config.fluid_factory == Some(emitter) {
        return fluid::decode_fluid_dex_deployed(log);
    }
    None
}

/// Fold one receipt-derived log into a batch result: DEX activity hits are
/// classified exactly as in `getLogs` discovery, and factory creation logs are
/// decoded with the same per-DEX decoders.
fn absorb_receipt_log(
    log: &alloy::rpc::types::Log,
    config: &DiscoveryConfig<'_>,
    into: &mut ScanBatchResult,
) {
    process_discovery_log(log, &mut into.active_blocks, &mut into.pool_hits);
    if let Some((addr, pool)) = decode_factory_creation_log(log, config) {
        // These three scanners also recorded activity hits in the existing
        // `getLogs` path; mirror that so metadata fetch behaves identically.
        if matches!(
            pool.dex_type,
            DexType::Balancer | DexType::Curve | DexType::TraderJoeLB
        ) {
            into.pool_hits.entry(addr).or_insert(PoolHit {
                dex_type: pool.dex_type,
                pool_id: pool.pool_id,
                tokens: None,
                first_seen_block: pool.creation_block,
            });
        }
        into.factory_pools.entry(addr).or_insert(pool);
    }
}

// ── Module-level macro: fetch logs with optional provider pinning ──
macro_rules! get_logs_pinned {
    ($rpc:expr, $filter:expr, $provider_idx:expr) => {
        match $provider_idx {
            Some(idx) => $rpc.get_logs_for(idx, $filter).await,
            None => $rpc.get_logs($filter).await,
        }
    };
}

// ── Per-DEX scanner submodules ──
mod balancer;
mod camelot;
mod curve;
mod factories;
mod fluid;
mod health;
mod infinity;
mod metadata;
mod metric;
mod pendle;
mod protocol_names;
pub mod remote;
mod solidly;
mod trader_joe;
mod v2;
mod v3;
mod v4;

pub use factories::{DiscoveryRuntimeOpts, ResolvedFactories};
pub use protocol_names::{is_epoch_venue_factory, protocol_name_for_factory, resolve_dex_name};

/// Unified pool discovery — scans both DEX activity events and factory
/// creation events (if factory addresses provided).
///
/// Distributes `getLogs` batches across RPC providers (like `distribute_blocks`
/// in fetch) to avoid overwhelming a single provider. Each provider handles
/// its assigned sub-range independently.
///
/// # DEX activity (always)
/// Scans for Swap/Sync/Mint/Burn/TokenExchange/BalancerSwap events across
/// all contracts, collecting unique emitting pool addresses. This captures
/// all pools that were *active* in the block range regardless of when they
/// were created.
///
/// # Factory creation (optional)
/// If `v2_factories`/`v3_factories`/`balancer_vault`/`curve_registry` are
/// provided, also scans for PairCreated/PoolCreated/PoolRegistered/PoolAdded
/// events. This captures pools created in the range that may have had zero
/// activity yet.
///
/// ## Type-specific behavior
///
/// | DEX | RPC calls needed per pool | Notes |
/// |-----|--------------------------|-------|
/// | V2 | `token0`, `token1` | Fee defaults to 30 bps or `v2_fee_override` |
/// | V3 | `token0`, `token1`, `fee`, `tickSpacing` | Full metadata on-chain |
/// | Curve | (none — populated by `init_from_rpc`) | Requires `fetch_curve_state` during pool init |
/// | Balancer | (none for tokens — from event topics) | Requires vault from config for full state |
///
/// Pools discovered via factory creation events carry `creation_block` and
/// `factory` metadata that helps `init_pools` skip non-existent pools.
pub async fn discover_pools(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    config: &DiscoveryConfig<'_>,
    on_batch: Option<&dyn Fn() -> bool>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    // ── Auto-detect RPC eth_getLogs block range limit ──
    let effective_batch_size = probe_get_logs_limit(rpc, to_block, config.batch_size).await;
    if effective_batch_size != config.batch_size {
        tracing::info!(
            "Adapted batch_size: {} → {} (RPC eth_getLogs limit detected)",
            config.batch_size,
            effective_batch_size
        );
    }
    let requested_factory = config.factory_batch_size.max(config.batch_size);
    let factory_batch_size = if requested_factory == config.batch_size {
        effective_batch_size
    } else {
        probe_get_logs_limit(rpc, to_block, requested_factory)
            .await
            .max(effective_batch_size)
    };

    // ── Distribute block range across providers (like fetch) ──
    let shards = rpc.distribute_blocks(from_block, to_block).await;
    let num_shards = shards.len();

    if num_shards <= 1 {
        // Single provider — run sequentially (no parallelism overhead)
        tracing::info!("Discovery: using 1 provider shard");
        return discover_pools_shard(ShardRequest {
            rpc,
            from_block,
            to_block,
            effective_batch_size,
            factory_batch_size,
            config,
            on_batch,
            provider_idx: None,
        })
        .await;
    }

    tracing::info!(
        "Discovery: distributing {} blocks across {} provider shards",
        to_block - from_block + 1,
        num_shards
    );

    // ── Run each shard in parallel ──
    use futures::stream::{self, StreamExt};
    let rpc_concurrency = config.rpc_concurrency;

    let shard_tasks: Vec<_> = shards
        .into_iter()
        .map(|shard| {
            let rpc = rpc.clone();
            let config_ref = config;
            let batch_size = effective_batch_size;
            let factory_batch = factory_batch_size;
            async move {
                discover_pools_shard(ShardRequest {
                    rpc: &rpc,
                    from_block: shard.from,
                    to_block: shard.to,
                    effective_batch_size: batch_size,
                    factory_batch_size: factory_batch,
                    config: config_ref,
                    on_batch: None,
                    provider_idx: Some(shard.provider_idx),
                })
                .await
            }
        })
        .collect();

    let shard_results: Vec<_> = stream::iter(shard_tasks)
        .buffer_unordered(rpc_concurrency.min(num_shards))
        .collect()
        .await;

    // ── Merge shard results ──
    let mut active_blocks = HashSet::new();
    let mut pool_hits: PoolHits = HashMap::new();
    let mut factory_pools: HashMap<Address, DiscoveredPool> = HashMap::new();

    for result in shard_results {
        match result {
            Ok((shard_pools, shard_active)) => {
                active_blocks.extend(shard_active);
                // pool_hits and factory_pools are not returned from shard,
                // so we collect them from the pools
                for pool in shard_pools {
                    let addr = pool.address;
                    let entry = pool_hits.entry(addr).or_insert(PoolHit {
                        dex_type: pool.dex_type,
                        pool_id: pool.pool_id,
                        tokens: None,
                        first_seen_block: pool.creation_block,
                    });
                    if pool.creation_block > 0 && pool.creation_block < entry.first_seen_block {
                        entry.first_seen_block = pool.creation_block;
                    }
                    factory_pools.entry(addr).or_insert(pool);
                }
            }
            Err(e) => {
                tracing::warn!("Shard failed: {e:#}");
            }
        }
    }

    if let Some(ref f) = on_batch {
        if !f() {
            anyhow::bail!("job cancelled");
        }
    }

    let pools: Vec<DiscoveredPool> = factory_pools.into_values().collect();
    Ok((pools, active_blocks))
}

/// Resolve missing Balancer `pool_id` for event-discovered pools via the vault.
async fn resolve_balancer_pool_ids(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    pool_hits: &mut PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
) {
    let Some(vault) = config.balancer_vault else {
        return;
    };
    let balancer_to_resolve: Vec<Address> = pool_hits
        .iter()
        .filter(|(_, hit)| hit.dex_type == DexType::Balancer && hit.pool_id.is_none())
        .map(|(addr, _)| *addr)
        .collect();

    if balancer_to_resolve.is_empty() {
        return;
    }

    tracing::info!(
        "Resolving pool_id for {} Balancer pools discovered via events...",
        balancer_to_resolve.len()
    );
    let resolved =
        balancer::resolve_pool_ids(rpc, vault, balancer_to_resolve, config.rpc_concurrency).await;

    let mut resolved_count = 0u32;
    for (addr, pool_id, tokens) in resolved {
        if let Some(entry) = pool_hits.get_mut(&addr) {
            entry.pool_id = Some(pool_id);
            resolved_count += 1;
        }
        if let Some(fp) = factory_pools.get_mut(&addr) {
            fp.pool_id = Some(pool_id);
            if tokens.is_some() {
                fp.underlying_tokens = tokens;
            }
        }
    }
    if resolved_count > 0 {
        tracing::info!("Resolved pool_id for {} Balancer pools", resolved_count);
    }
}

/// Fetch full Curve token lists for registry-discovered pools via `coins(i)`.
async fn resolve_curve_underlying(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
) {
    let curve_pools: Vec<Address> = factory_pools
        .iter()
        .filter(|(_, fp)| fp.dex_type == DexType::Curve && fp.underlying_tokens.is_none())
        .map(|(addr, _)| *addr)
        .collect();

    if curve_pools.is_empty() {
        return;
    }

    tracing::info!(
        "Resolving underlying tokens for {} Curve pools...",
        curve_pools.len()
    );
    let resolved = curve::resolve_underlying_tokens(rpc, curve_pools, config.rpc_concurrency).await;

    let mut resolved_count = 0u32;
    for (addr, tokens) in resolved {
        if let Some(fp) = factory_pools.get_mut(&addr) {
            fp.underlying_tokens = Some(tokens);
            resolved_count += 1;
        }
    }
    if resolved_count > 0 {
        tracing::info!(
            "Resolved underlying tokens for {} Curve pools",
            resolved_count
        );
    }
}

/// Build metadata fetch tasks for event-discovered pools and run them with bounded concurrency.
async fn fetch_dex_metadata(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    pool_hits: &PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
) -> Vec<metadata::PoolMetadataFetch> {
    let mut fetch_tasks: Vec<metadata::FetchTask> = Vec::new();
    let mut cache_hits: usize = 0;
    let mut unmatched_v4_hits: usize = 0;

    let pool_hits_vec: Vec<(Address, PoolHit)> =
        pool_hits.iter().map(|(addr, hit)| (*addr, *hit)).collect();

    for (addr, hit) in &pool_hits_vec {
        if let Some(fp) = factory_pools.get(addr) {
            match fp.dex_type {
                DexType::UniswapV2
                | DexType::UniswapV3
                | DexType::UniswapV4
                | DexType::PancakeInfinity
                | DexType::TraderJoeLB
                | DexType::Pendle
                | DexType::Metric => continue,
                _ => {}
            }
        }
        if let Some(cached) = config
            .pool_cache
            .and_then(|c| c.get_discovered_pool(addr).ok().flatten())
        {
            if cached.token0 != Address::ZERO && cached.token1 != Address::ZERO {
                let dp = DiscoveredPool::new(
                    cached.address,
                    cached.token0,
                    cached.token1,
                    cached.fee,
                    cached.dex_type,
                    cached.creation_block,
                )
                .with_tick_spacing(cached.tick_spacing.map(|ts| ts as i32))
                .with_pool_id(cached.pool_id)
                .with_factory(cached.factory)
                .with_is_stable(cached.is_stable)
                .with_balancer_pool_type(cached.balancer_pool_type)
                .with_hook_address(cached.hook_address)
                .with_bin_step(cached.bin_step)
                .with_maturity_timestamp(cached.maturity_timestamp)
                .with_underlying_tokens(cached.underlying_tokens)
                .with_dex_name(cached.dex_name.as_deref().map(String::from))
                .with_token0_symbol(cached.token0_symbol.as_deref().map(String::from))
                .with_token1_symbol(cached.token1_symbol.as_deref().map(String::from));
                factory_pools.entry(*addr).or_insert(dp);
                cache_hits += 1;
                continue;
            }
        }
        let factory_tokens = factory_pools.get(addr).map(|fp| (fp.token0, fp.token1));
        match metadata::fetch_pool_metadata(
            rpc,
            *addr,
            hit.dex_type,
            hit.tokens,
            hit.first_seen_block,
            factory_tokens,
        ) {
            Some(task) => fetch_tasks.push(task),
            None => unmatched_v4_hits += 1,
        }
    }

    let rpc_tasks = fetch_tasks.len();
    if cache_hits > 0 {
        tracing::info!(
            "Pool cache: {} of {} pools served from cache ({} RPC tasks remaining)",
            cache_hits,
            pool_hits_vec.len(),
            rpc_tasks,
        );
    }
    if unmatched_v4_hits > 0 {
        tracing::info!(
            "V4 activity: {unmatched_v4_hits} poolId hit(s) without Initialize/cache match — skipped (no RPC spent)"
        );
    }

    let rpc_concurrency = config.rpc_concurrency;
    use futures::stream::{self, StreamExt};

    stream::iter(fetch_tasks)
        .buffer_unordered(rpc_concurrency)
        .collect::<Vec<_>>()
        .await
}

/// Resolve ERC-20 symbols for all tokens referenced by factory pools and metadata results.
async fn resolve_token_symbols(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    factory_pools: &HashMap<Address, DiscoveredPool>,
    results: &[metadata::PoolMetadataFetch],
) -> HashMap<Address, String> {
    let symbol_selector = SYMBOL.clone();
    let mut token_addrs: HashSet<Address> = HashSet::new();
    for fp in factory_pools.values() {
        token_addrs.insert(fp.token0);
        token_addrs.insert(fp.token1);
    }
    for r in results {
        if let Some(t) = r.token0 {
            token_addrs.insert(t);
        }
        if let Some(t) = r.token1 {
            token_addrs.insert(t);
        }
    }
    token_addrs.remove(&Address::ZERO);

    let token_vec: Vec<Address> = token_addrs.into_iter().collect();

    let (cached_symbols, uncached_tokens) = if let Some(cache) = config.token_cache {
        let mut cached = HashMap::new();
        let mut uncached = Vec::new();
        for &addr in &token_vec {
            if let Some(sym) = cache.get(&addr) {
                cached.insert(addr, sym.to_string());
            } else {
                uncached.push(addr);
            }
        }
        tracing::info!(
            "Token cache: {}/{} tokens cached, {} need RPC resolution",
            cached.len(),
            token_vec.len(),
            uncached.len()
        );
        (cached, uncached)
    } else {
        (HashMap::new(), token_vec.clone())
    };

    let symbol_tasks: Vec<_> = uncached_tokens
        .iter()
        .map(|addr| {
            let rpc = rpc.clone();
            let addr = *addr;
            let sel = symbol_selector.clone();
            async move {
                let sym = rpc
                    .call_latest(addr, sel)
                    .await
                    .ok()
                    .and_then(|b| decode_abi_string(&b));
                (addr, sym)
            }
        })
        .collect();

    let rpc_concurrency = config.rpc_concurrency;
    use futures::stream::{self, StreamExt};

    let rpc_resolved: HashMap<Address, String> = if symbol_tasks.is_empty() {
        HashMap::new()
    } else {
        stream::iter(symbol_tasks)
            .buffer_unordered(rpc_concurrency)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|(addr, sym)| sym.map(|s| (addr, s)))
            .collect()
    };

    let mut symbol_results: HashMap<Address, String> = cached_symbols;
    let rpc_resolved_count = rpc_resolved.len();
    for (addr, sym) in rpc_resolved {
        symbol_results.insert(addr, sym);
    }

    let cached_count = symbol_results.len() - rpc_resolved_count;
    tracing::info!(
        "Resolved symbols for {}/{} tokens ({} cached, {} from RPC)",
        symbol_results.len(),
        token_vec.len(),
        cached_count,
        rpc_resolved_count
    );

    symbol_results
}

/// Merge factory-discovered pools and metadata fetch results into the final output list.
fn build_discovery_output(
    config: &DiscoveryConfig<'_>,
    pool_hits: &PoolHits,
    mut factory_pools: HashMap<Address, DiscoveredPool>,
    results: Vec<metadata::PoolMetadataFetch>,
    symbol_results: HashMap<Address, String>,
) -> Vec<DiscoveredPool> {
    let mut resolved_addrs: HashSet<Address> = HashSet::new();
    let mut discovered_pools = Vec::new();

    for r in &results {
        if r.dex_type != DexType::Fluid {
            continue;
        }
        if let Some(fp) = factory_pools.get_mut(&r.addr) {
            if let (Some(t0), Some(t1)) = (r.token0, r.token1) {
                if fp.token0.is_zero() && fp.token1.is_zero() {
                    fp.token0 = t0;
                    fp.token1 = t1;
                }
            }
        }
    }

    for (_, mut dp) in factory_pools.drain() {
        if dp.dex_name.is_none() {
            dp.dex_name = Some(resolve_dex_name(dp.factory, &dp.dex_type.to_string()));
        }
        dp.token0_symbol = symbol_results.get(&dp.token0).cloned();
        dp.token1_symbol = symbol_results.get(&dp.token1).cloned();
        resolved_addrs.insert(dp.address);
        discovered_pools.push(dp);
    }

    let mut degraded_count = 0u32;
    for r in results {
        if !resolved_addrs.insert(r.addr) {
            continue;
        }
        let addr = r.addr;
        let dex_type = r.dex_type;
        let fee_opt = r.fee;
        let tick_spacing = r.tick_spacing;
        let first_seen_block = r.first_seen_block;
        let requires_tokens = matches!(
            dex_type,
            DexType::UniswapV2
                | DexType::Solidly
                | DexType::Camelot
                | DexType::Fluid
                | DexType::TraderJoeLB
                | DexType::UniswapV3
                | DexType::UniswapV4
                | DexType::PancakeInfinity
        );
        if requires_tokens && (r.token0.is_none() || r.token1.is_none()) {
            degraded_count += 1;
        }
        let token0 = r.token0.unwrap_or(Address::ZERO);
        let token1 = r.token1.unwrap_or(Address::ZERO);
        let fee = metadata::default_fee(dex_type, fee_opt, config.v2_fee_override);
        let pool_id = pool_hits.get(&addr).and_then(|hit| hit.pool_id);
        let creation_block = pool_hits
            .get(&addr)
            .map(|hit| hit.first_seen_block)
            .unwrap_or(first_seen_block);
        let underlying_tokens = factory_pools
            .get(&addr)
            .and_then(|fp| fp.underlying_tokens.clone());

        discovered_pools.push(
            DiscoveredPool::new(addr, token0, token1, fee, dex_type, creation_block)
                .with_tick_spacing(tick_spacing.map(|ts| ts as i32))
                .with_pool_id(pool_id)
                .with_underlying_tokens(underlying_tokens)
                .with_dex_name(Some(resolve_dex_name(None, &dex_type.to_string())))
                .with_token0_symbol(symbol_results.get(&token0).cloned())
                .with_token1_symbol(symbol_results.get(&token1).cloned()),
        );
    }

    tracing::info!(
        "Discovery complete: resolved {} pools",
        discovered_pools.len(),
    );
    if degraded_count > 0 {
        tracing::warn!(
            "Discovery: {degraded_count} pool(s) had RPC metadata failures — token/fee fields fall back to defaults"
        );
    }

    discovered_pools
}

/// Discovery logic for a single block-range shard (pinned to one provider).
fn block_windows(from: u64, to: u64, batch: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    if from > to {
        return out;
    }
    let batch = batch.max(1);
    let mut current = from;
    while current <= to {
        let end = (current + batch - 1).min(to);
        out.push((current, end));
        if end == to {
            break;
        }
        current = end + 1;
    }
    out
}

fn absorb_activity_logs(logs: &[alloy::rpc::types::Log], into: &mut ScanBatchResult) {
    for log in logs {
        process_discovery_log(log, &mut into.active_blocks, &mut into.pool_hits);
    }
}

async fn scan_activity_window(
    rpc: &RpcClient,
    topics: &[B256],
    from: u64,
    to: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    let mut result = ScanBatchResult::default();
    let filter = Filter::new()
        .event_signature(topics.to_vec())
        .from_block(from)
        .to_block(to);
    match get_logs_pinned!(rpc, &filter, provider_idx) {
        Ok(logs) => absorb_activity_logs(&logs, &mut result),
        Err(e) => {
            tracing::warn!(
                "DEX activity scan failed for blocks {from}..{to}: {e:#}. Splitting batch..."
            );
            match get_logs_split_recursive(rpc, &filter, from, to).await {
                Ok(logs) => absorb_activity_logs(&logs, &mut result),
                Err(e2) => {
                    tracing::warn!(
                        "Activity scan split also failed for blocks {from}..{to}: {e2:#}. \
                         Skipping batch."
                    );
                }
            }
        }
    }
    result
}

async fn scan_activity_windows(
    rpc: &RpcClient,
    topics: &[B256],
    windows: Vec<(u64, u64)>,
    provider_idx: Option<usize>,
    concurrency: usize,
) -> ScanBatchResult {
    if concurrency <= 1 || windows.len() <= 1 {
        let mut merged = ScanBatchResult::default();
        for (from, to) in windows {
            merged.merge(scan_activity_window(rpc, topics, from, to, provider_idx).await);
        }
        return merged;
    }
    use futures::stream::{self, StreamExt};
    let parts: Vec<ScanBatchResult> = stream::iter(windows)
        .map(|(from, to)| {
            let topics = topics.to_vec();
            let rpc = rpc.clone();
            async move { scan_activity_window(&rpc, &topics, from, to, provider_idx).await }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let mut merged = ScanBatchResult::default();
    for part in parts {
        merged.merge(part);
    }
    merged
}

/// One provider shard of a discovery sweep: block range plus resolved batch
/// sizes, config and provider pin.
struct ShardRequest<'a, 'c> {
    rpc: &'a RpcClient,
    from_block: u64,
    to_block: u64,
    effective_batch_size: u64,
    factory_batch_size: u64,
    config: &'a DiscoveryConfig<'c>,
    on_batch: Option<&'a dyn Fn() -> bool>,
    provider_idx: Option<usize>,
}

async fn discover_pools_shard(
    req: ShardRequest<'_, '_>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let ShardRequest {
        rpc,
        from_block,
        to_block,
        effective_batch_size,
        factory_batch_size,
        config,
        on_batch,
        provider_idx,
    } = req;
    let mut active_blocks = HashSet::new();
    let mut pool_hits: PoolHits = HashMap::new();
    let mut factory_pools: HashMap<Address, DiscoveredPool> = HashMap::new();

    // Pre-build the DEX activity topic list.
    //

    // Single unified filter across all pool-level event types. Previously this
    // was split into a "fast" V2/V3-only path with Curve/Balancer topics only
    // queried on failure — which meant Curve/Balancer/LB/Pendle *activity*
    // discovery almost never ran. V4 / Pancake-Infinity Swap logs are included
    // but carry only a bytes32 poolId (singleton PoolManagers): hits are matched
    // against the Initialize-event scan / SQLite cache and are never resolved
    // through per-pool metadata calls.
    let dex_activity_topics: Vec<B256> = vec![
        topics::V2_SWAP,
        topics::V2_SYNC,
        topics::V3_SWAP,
        topics::V3_MINT,
        topics::V3_BURN,
        *topics::CURVE_TOKEN_EXCHANGE,
        *topics::CURVE_V2_TOKEN_EXCHANGE,
        *topics::CURVE_TOKEN_EXCHANGE_UNDERLYING,
        *topics::CURVE_V2_TOKEN_EXCHANGE_UNDERLYING,
        *topics::BALANCER_SWAP,
        *topics::TRADER_JOE_LB_SWAP,
        *topics::TRADER_JOE_LB_SWAP_LEGACY,
        *topics::PENDLE_MARKET_SWAP,
        *topics::PENDLE_SWAP_PT_AND_SY,
        *topics::PENDLE_SWAP_YT_AND_SY,
        *topics::PENDLE_SWAP_PT_AND_TOKEN,
        *topics::PENDLE_SWAP_YT_AND_TOKEN,
        *topics::V4_SWAP,
        *topics::INF_CL_SWAP,
        *topics::FLUID_SWAP,
        *topics::METRIC_SWAP,
    ];

    let unlimited = match provider_idx {
        Some(idx) => rpc.provider_is_unlimited(idx).await,
        None => rpc.has_unlimited_provider().await,
    };
    // Rate-limited shards stay serial so a 1 RPS bucket is not fanned out.
    let window_conc = if unlimited {
        config.rpc_concurrency.max(1)
    } else {
        1
    };
    let factory_step = factory_batch_size.max(effective_batch_size).max(1);

    let mut current = from_block;

    while current <= to_block {
        let batch_end = (current + factory_step - 1).min(to_block);

        // ── Provider health check ──
        if !rpc.has_healthy_providers().await {
            tracing::warn!(
                "All RPC providers in cooldown before batch {current}..{batch_end}, waiting 5s..."
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        // ── DEX activity scan (parallel only on an unlimited provider) ──
        let activity = scan_activity_windows(
            rpc,
            &dex_activity_topics,
            block_windows(current, batch_end, effective_batch_size),
            provider_idx,
            window_conc,
        )
        .await;
        active_blocks.extend(activity.active_blocks);
        for (addr, hit) in activity.pool_hits {
            pool_hits.entry(addr).or_insert(hit);
        }

        // ── Per-DEX factory scans, run concurrently over the coarse window ──
        // Each scanner returns its additions and the results are merged
        // afterwards; previously they serialized the whole batch on the
        // slowest provider.
        use futures::future::{join_all, BoxFuture};
        let scan_ctx = ScanContext {
            rpc,
            config,
            current,
            batch_end,
            provider_idx,
        };
        let scanners: Vec<BoxFuture<'_, ScanBatchResult>> = vec![
            Box::pin(v2::scan_v2_batch(&scan_ctx)),
            Box::pin(v3::scan_v3_batch(&scan_ctx)),
            Box::pin(balancer::scan_balancer_batch(&scan_ctx)),
            Box::pin(curve::scan_curve_batch(&scan_ctx)),
            Box::pin(solidly::scan_solidly_batch(&scan_ctx)),
            Box::pin(camelot::scan_camelot_batch(&scan_ctx)),
            Box::pin(trader_joe::scan_trader_joe_batch(&scan_ctx)),
            Box::pin(pendle::scan_pendle_batch(&scan_ctx)),
            Box::pin(v4::scan_v4_batch(&scan_ctx)),
            Box::pin(infinity::scan_infinity_cl_batch(&scan_ctx)),
            Box::pin(metric::scan_metric_batch(&scan_ctx)),
            Box::pin(fluid::scan_fluid_batch(&scan_ctx)),
        ];
        let scanner_results = join_all(scanners).await;
        for result in scanner_results {
            active_blocks.extend(result.active_blocks);
            for (addr, hit) in result.pool_hits {
                pool_hits.entry(addr).or_insert(hit);
            }
            for (addr, pool) in result.factory_pools {
                factory_pools.entry(addr).or_insert(pool);
            }
        }

        if let Some(ref f) = on_batch {
            if !f() {
                anyhow::bail!("job cancelled");
            }
        }

        if batch_end == to_block {
            break;
        }
        current = batch_end + 1;
    }

    if pool_hits.is_empty() && factory_pools.is_empty() {
        return Ok((Vec::new(), active_blocks));
    }

    tracing::info!(
        "Event scan: found {} unique pool addresses from DEX events, {} from factory events, {} active blocks",
        pool_hits.len(),
        factory_pools.len(),
        active_blocks.len(),
    );

    resolve_balancer_pool_ids(rpc, config, &mut pool_hits, &mut factory_pools).await;
    resolve_curve_underlying(rpc, config, &mut factory_pools).await;

    let results = fetch_dex_metadata(rpc, config, &pool_hits, &mut factory_pools).await;
    let symbol_results = resolve_token_symbols(rpc, config, &factory_pools, &results).await;
    let discovered_pools =
        build_discovery_output(config, &pool_hits, factory_pools, results, symbol_results);

    Ok((discovered_pools, active_blocks))
}

/// Discover pools and save them to the cache.
/// Called by `mev-scout discover`. `live` only reads pools already cached.
pub async fn discover_and_cache(
    rpc: &RpcClient,
    cache: &SqliteStore,
    from_block: u64,
    to_block: u64,
    config: &DiscoveryConfig<'_>,
    on_batch: Option<&dyn Fn() -> bool>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let (pools, active_blocks) =
        discover_pools(rpc, from_block, to_block, config, on_batch).await?;
    persist_discovered_pools(cache, &pools, config.token_cache);

    Ok((pools, active_blocks))
}

/// Save newly discovered pools and their token symbols to the SQLite caches.
/// Shared by the `getLogs` and receipts discovery paths so both persist
/// identical rows for identical `DiscoveredPool` values.
pub fn persist_discovered_pools(
    cache: &SqliteStore,
    pools: &[DiscoveredPool],
    token_cache: Option<&crate::cache::TokenCache>,
) {
    // Save newly discovered pool symbols to the token cache
    if let Some(token_cache) = token_cache {
        let mut new_tokens: Vec<(Address, String, Option<i32>)> = Vec::new();
        for pool in pools {
            if let Some(ref sym) = pool.token0_symbol {
                if !token_cache.contains(&pool.token0) {
                    new_tokens.push((pool.token0, sym.clone(), None));
                }
            }
            if let Some(ref sym) = pool.token1_symbol {
                if !token_cache.contains(&pool.token1) {
                    new_tokens.push((pool.token1, sym.clone(), None));
                }
            }
        }
        if !new_tokens.is_empty() {
            if let Err(e) = token_cache.save_batch(cache, &new_tokens) {
                tracing::warn!("Token cache: failed to save new symbols: {e:#}");
            }
        }
    }

    let pool_count = pools.len();
    let mut persisted = 0usize;
    let mut skipped_zero = 0usize;
    for pool in pools {
        // Zero-token entries (e.g. V4 activity hits without an Initialize/cache
        // match, or Curve/Balancer hits whose tokens could not be resolved)
        // would poison the pool universe downstream — never persist them.
        if pool.token0.is_zero() || pool.token1.is_zero() {
            skipped_zero += 1;
            continue;
        }
        let info: PoolInfo = pool.clone().into();
        if let Err(e) = cache.put_discovered_pool(&info) {
            tracing::warn!("Failed to cache pool {}: {}", pool.address, e);
        } else {
            persisted += 1;
        }
    }
    if persisted > 0 {
        tracing::info!("Cached {persisted}/{pool_count} pools from discovery");
    }
    if skipped_zero > 0 {
        // Observable signal for a flaky RPC or hit-to-metadata gap: these
        // pools were silently dropped rather than corrupting the universe.
        tracing::warn!(
            "Discovery: {skipped_zero}/{pool_count} pools skipped (unresolved/zero tokens) — check RPC health"
        );
    }
}

/// Convert an internal cached `LogData` (as produced by `eth_getBlockReceipts`)
/// into the alloy log shape the shared decoders consume.
fn receipt_log_to_rpc_log(
    log: &crate::data::types::LogData,
    block_number: u64,
    tx_index: u64,
    fallback_log_index: u64,
) -> alloy::rpc::types::Log {
    alloy::rpc::types::Log {
        inner: alloy::primitives::Log {
            address: log.address,
            data: alloy::primitives::LogData::new_unchecked(
                log.topics.clone(),
                log.data.clone(),
            ),
        },
        block_number: Some(block_number),
        block_hash: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: Some(tx_index),
        log_index: log.log_index.or(Some(fallback_log_index)),
        removed: false,
    }
}

/// Options for the receipts-based scan.
pub struct ReceiptScanOptions<'a> {
    /// Blocks fetched in one `eth_getBlockReceipts` batch POST (default: 4).
    pub batch_blocks: u64,
    /// Cache fetched block/receipt bundles in SQLite so `get_receipts` works
    /// on later runs. Off by default: a 30-day scan needs significant disk.
    pub cache_blocks: bool,
    /// Pin every receipt batch to one provider (used by benchmark M1/M2).
    pub provider_idx: Option<usize>,
    /// Mixed providers: `(blocks_from_tip, recent_idx, old_idx)`. Windows in
    /// the trailing `blocks_from_tip` blocks use `recent_idx`, older ones use
    /// `old_idx` (benchmark M3).
    pub mixed_recent: Option<(u64, usize, usize)>,
    /// Maximum number of block windows fetched concurrently.
    pub concurrency: usize,
    /// Called once per block window, before the RPC request.
    pub on_window: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
}

/// Outcome of a receipts scan. `failed_windows` is the exact list of ranges
/// that must be re-scanned: callers should only advance the discovery
/// watermark when it is empty.
pub struct ReceiptScanResult {
    pub pools: Vec<DiscoveredPool>,
    pub active_blocks: HashSet<u64>,
    pub failed_windows: Vec<(u64, u64)>,
}

/// Discover pools without `eth_getLogs`: fetch block receipts in batches and
/// run the same decoders as the `getLogs` scanners over every local log.
///
/// The rest of the pipeline (metadata, symbols, output building) is unchanged;
/// only the log source differs.
pub async fn discover_pools_from_receipts(
    rpc: &RpcClient,
    cache: &SqliteStore,
    from_block: u64,
    to_block: u64,
    config: &DiscoveryConfig<'_>,
    opts: ReceiptScanOptions<'_>,
) -> anyhow::Result<ReceiptScanResult> {
    struct WindowOutcome {
        from: u64,
        to: u64,
        batch: ScanBatchResult,
        failed: bool,
    }

    let batch_blocks = opts.batch_blocks.max(1);
    let cache_blocks = opts.cache_blocks;
    let pinned = opts.provider_idx;
    let mixed_recent = opts.mixed_recent;
    let on_window = opts.on_window;
    let concurrency = opts.concurrency.max(1);

    let outcomes: Vec<anyhow::Result<WindowOutcome>> = stream::iter(block_windows(
        from_block,
        to_block,
        batch_blocks,
    ))
    .map(|(from, to)| {
        let rpc = rpc;
        let cache = cache;
        let config = config;
        async move {
            if let Some(cb) = on_window {
                if !cb() {
                    anyhow::bail!("job cancelled");
                }
            }

            let blocks: Vec<u64> = (from..=to).collect();
            let provider_idx = pinned.or_else(|| {
                mixed_recent.map(|(recent_blocks, recent_idx, old_idx)| {
                    let cutoff = to_block.saturating_sub(recent_blocks);
                    if to >= cutoff {
                        recent_idx
                    } else {
                        old_idx
                    }
                })
            });
            let bundles = match provider_idx {
                Some(idx) => rpc.get_blocks_and_receipts_batch_for_many(idx, &blocks).await,
                None => rpc.get_blocks_and_receipts_batch(&blocks).await,
            };

            let bundles = match bundles {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("Receipt scan failed for {from}..{to}: {e:#}");
                    return Ok(WindowOutcome {
                        from,
                        to,
                        batch: ScanBatchResult::default(),
                        failed: true,
                    });
                }
            };

            let mut batch = ScanBatchResult::default();
            for (block, _txs, receipts) in &bundles {
                let block_num = block.number;
                for receipt in receipts {
                    for (log_idx, log) in receipt.logs.iter().enumerate() {
                        let rpc_log = receipt_log_to_rpc_log(
                            log,
                            block_num,
                            receipt.tx_index,
                            log.log_index.unwrap_or(log_idx as u64),
                        );
                        absorb_receipt_log(&rpc_log, config, &mut batch);
                    }
                }
            }

            if cache_blocks {
                let cache_rows: Vec<_> = bundles
                    .iter()
                    .map(|(block, txs, receipts)| {
                        (block.number, block.clone(), txs.clone(), receipts.clone())
                    })
                    .collect();
                if let Err(e) = cache.put_block_data_batch(&cache_rows, None, None) {
                    tracing::warn!("Failed to cache receipt bundles {from}..{to}: {e:#}");
                }
            }

            Ok(WindowOutcome {
                from,
                to,
                batch,
                failed: false,
            })
        }
    })
    .buffered(concurrency)
    .collect()
    .await;

    let mut active_blocks = HashSet::new();
    let mut pool_hits: PoolHits = HashMap::new();
    let mut factory_pools: HashMap<Address, DiscoveredPool> = HashMap::new();
    let mut failed_windows = Vec::new();
    // `buffered` preserves input order, so first-wins merges stay deterministic.
    for outcome in outcomes {
        let outcome = outcome?;
        active_blocks.extend(outcome.batch.active_blocks);
        for (addr, hit) in outcome.batch.pool_hits {
            pool_hits.entry(addr).or_insert(hit);
        }
        for (addr, pool) in outcome.batch.factory_pools {
            factory_pools.entry(addr).or_insert(pool);
        }
        if outcome.failed {
            failed_windows.push((outcome.from, outcome.to));
        }
    }

    if pool_hits.is_empty() && factory_pools.is_empty() {
        return Ok(ReceiptScanResult {
            pools: Vec::new(),
            active_blocks,
            failed_windows,
        });
    }

    tracing::info!(
        "Receipt scan: {} unique pool addresses from DEX activity, {} from factory events, {} active blocks",
        pool_hits.len(),
        factory_pools.len(),
        active_blocks.len(),
    );

    resolve_balancer_pool_ids(rpc, config, &mut pool_hits, &mut factory_pools).await;
    resolve_curve_underlying(rpc, config, &mut factory_pools).await;
    let results = fetch_dex_metadata(rpc, config, &pool_hits, &mut factory_pools).await;
    let symbol_results = resolve_token_symbols(rpc, config, &factory_pools, &results).await;
    let pools = build_discovery_output(config, &pool_hits, factory_pools, results, symbol_results);

    Ok(ReceiptScanResult {
        pools,
        active_blocks,
        failed_windows,
    })
}

/// Post-discovery health check: queries on-chain state for V2/Solidly/Camelot
/// pools (getReserves) and V3 pools (liquidity) to filter out drained or paused
/// pools. Only pools with non-zero reserves/liquidity are kept.
///
/// Returns the filtered list and a count of removed pools.
pub async fn health_check_pools(
    rpc: &RpcClient,
    pools: Vec<DiscoveredPool>,
    rpc_concurrency: usize,
    balancer_vault: Option<Address>,
) -> (Vec<DiscoveredPool>, usize) {
    use futures::stream::{self, StreamExt};

    // Build health check tasks: (index, to_address, calldata). The per-DEX
    // call layout lives in discovery/health.rs.
    let mut tasks: Vec<(usize, Address, Bytes)> = Vec::new();
    for (i, pool) in pools.iter().enumerate() {
        if let Some(probe) = health::health_probe(pool, balancer_vault) {
            tasks.push((i, probe.to, probe.data));
        }
    }

    if tasks.is_empty() {
        return (pools, 0);
    }

    tracing::info!(
        "Health checking {} pools (concurrency={})...",
        tasks.len(),
        rpc_concurrency
    );

    // Get latest block for the call
    let block = match rpc.get_block_number().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "Health check: failed to get block number: {e:#}. Skipping health check."
            );
            return (pools, 0);
        }
    };

    // Pre-extract dex types so closures don't need to borrow `pools`
    let dex_types: Vec<DexType> = pools.iter().map(|p| p.dex_type).collect();

    /// Outcome of a single pool health probe.
    enum PoolHealth {
        Alive,
        Dead,
        Unknown,
    }

    // Run health checks with bounded concurrency.
    // RPC errors are treated as *unknown* and the pool is kept: a flaky public
    // RPC must not silently delete healthy pools from the index.
    let check_results: Vec<(usize, PoolHealth)> = stream::iter(tasks)
        .map(|(idx, to, data)| {
            let rpc = rpc.clone();
            let dex_type = dex_types[idx];
            async move {
                let health = match rpc.call(to, data, block).await {
                    Ok(bytes) => {
                        if health::probe_alive(&dex_type, &bytes) {
                            PoolHealth::Alive
                        } else {
                            PoolHealth::Dead
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            "Health check failed for {}: {:#} — unverifiable, keeping pool",
                            to,
                            e
                        );
                        PoolHealth::Unknown
                    }
                };
                (idx, health)
            }
        })
        .buffer_unordered(rpc_concurrency)
        .collect()
        .await;

    // Collect results — only pools that were probed AND confirmed drained/paused
    // are removed. Unverified pools (RPC errors) stay in the index.
    let mut checked_set: HashSet<usize> = HashSet::new();
    let mut dead_set: HashSet<usize> = HashSet::new();
    let mut unknown_count = 0usize;
    for (idx, health) in check_results {
        checked_set.insert(idx);
        match health {
            PoolHealth::Alive => {}
            PoolHealth::Dead => {
                dead_set.insert(idx);
            }
            PoolHealth::Unknown => unknown_count += 1,
        }
    }

    let original_count = pools.len();
    let filtered: Vec<DiscoveredPool> = pools
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !checked_set.contains(i) || !dead_set.contains(i))
        .map(|(_, p)| p)
        .collect();
    let removed = original_count - filtered.len();
    if removed > 0 {
        tracing::info!(
            "Health check: removed {} drained/paused pools ({} remaining)",
            removed,
            filtered.len()
        );
    } else {
        tracing::info!("Health check: all {} pools healthy", filtered.len());
    }
    if unknown_count > 0 {
        tracing::warn!(
            "Health check: {} pool(s) unverifiable due to RPC errors — kept as-is",
            unknown_count
        );
    }

    (filtered, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::selectors::{TRADER_JOE_TOKEN_X, TRADER_JOE_TOKEN_Y};
    use alloy::primitives::{address, b256, LogData};

    fn make_log(address: Address, topics_vec: Vec<B256>, data: Vec<u8>) -> alloy::rpc::types::Log {
        let log_data = LogData::new_unchecked(topics_vec, Bytes::from(data));
        alloy::rpc::types::Log {
            inner: alloy::primitives::Log {
                address,
                data: log_data,
            },
            block_number: Some(1000),
            block_hash: None,
            block_timestamp: None,
            transaction_hash: Some(b256!(
                "0000000000000000000000000000000000000000000000000000000000000001"
            )),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        }
    }

    /// V4 Swap logs must classify as UniswapV4 with the poolId recovered from
    /// topics[1] and a synthetic pool key matching the v4.rs Initialize scan.
    #[test]
    fn classify_v4_swap_recovers_pool_id() {
        let pool_manager = address!("000000000000004444c5dc73dcbabcdd12345678");
        let pool_id = B256::from([
            0x12, 0x34, 0x56, 0x78, 0x90, 0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78, 0xaa, 0xbb,
            0xcc, 0xdd, 0xee, 0xff, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
            0xbb, 0xcc, 0xdd, 0xee,
        ]);
        let log = make_log(
            pool_manager,
            vec![*topics::V4_SWAP, pool_id],
            vec![0u8; 224],
        );

        let hit = classify_dex_event(&log).expect("V4 swap must classify");
        assert_eq!(hit.dex_type, DexType::UniswapV4);
        assert_eq!(hit.pool_id, Some(pool_id.0));
        assert!(hit.tokens.is_none());
        // Synthetic key = last 20 bytes of poolId (consistent with v4.rs)
        assert_eq!(
            hit.addr_override,
            crate::utils::abi_decode_address(pool_id.as_slice(), 0)
        );
    }

    /// Pancake Infinity CL Initialize has a distinct canonical signature from
    /// Uniswap V4's (uint24 fee + bytes32 parameters + uint160 sqrtPriceX96 +
    /// int24 tick instead of uint24 fee + int24 tickSpacing + address hooks),
    /// so the two manager topics must differ and never shadow each other.
    #[test]
    fn infinity_cl_initialize_topic_differs_from_v4() {
        use crate::chain::events::INF_CL_SWAP_TOPIC;
        assert_ne!(*INF_CL_INITIALIZE_TOPIC, *V4_INITIALIZE_TOPIC);
        assert_eq!(
            *INF_CL_INITIALIZE_TOPIC,
            keccak256(b"Initialize(bytes32,address,address,address,uint24,bytes32,uint160,int24)")
        );
        assert_ne!(*INF_CL_SWAP_TOPIC, *topics::V4_SWAP);
    }

    /// Infinity CL Swap logs must classify like V4: singleton emitter, poolId in
    /// topics[1], synthetic pool key from poolId[12..32].
    #[test]
    fn classify_infinity_cl_swap_recovers_pool_id() {
        let pool_manager = address!("a0FfB9c1CE1Fe56963B0321B32E7A0302114058b");
        let pool_id = B256::from([
            0x21, 0x43, 0x65, 0x87, 0xa9, 0xcb, 0xed, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc,
            0xfe, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78, 0x9a,
            0xbc, 0xde, 0xf0, 0x11,
        ]);
        let log = make_log(
            pool_manager,
            vec![*topics::INF_CL_SWAP, pool_id],
            vec![0u8; 128 + 32], // 7 data words incl. trailing protocolFee
        );

        let hit = classify_dex_event(&log).expect("Infinity CL swap must classify");
        assert_eq!(hit.dex_type, DexType::PancakeInfinity);
        assert_eq!(hit.pool_id, Some(pool_id.0));
        assert!(hit.tokens.is_none());
        assert_eq!(
            hit.addr_override,
            crate::utils::abi_decode_address(pool_id.as_slice(), 0)
        );
    }

    /// Trader Joe LBPair metadata getters are `tokenX`/`tokenY`; calling
    /// the standard `token0`/`token1` selectors on an LBPair reverts and
    /// would leave every activity-discovered LB pool without tokens.
    #[test]
    fn trader_joe_metadata_selectors_match_lbpair_abi() {
        assert_eq!(
            *TRADER_JOE_TOKEN_X,
            Bytes::from_static(&[0x16, 0xdc, 0x16, 0x5b])
        );
        assert_eq!(
            *TRADER_JOE_TOKEN_Y,
            Bytes::from_static(&[0xb7, 0xd1, 0x9f, 0xc4])
        );
        // Sanity: they must differ from the standard pair selectors.
        let token0 = keccak256(b"token0()");
        assert_ne!(*TRADER_JOE_TOKEN_X, Bytes::copy_from_slice(&token0[..4]));
    }

    /// Pendle router PT/YT swap logs must classify as Pendle with the hit
    /// address overridden by the market recovered from topics[2] — the emitter
    /// is the singleton router, never a pool.
    #[test]
    fn classify_pendle_router_swaps_override_with_market() {
        let market = address!("cccccccccccccccccccccccccccccccccccccccc");
        let caller = address!("1111111111111111111111111111111111111111");
        let receiver = address!("2222222222222222222222222222222222222222");

        for topic in [
            *topics::PENDLE_SWAP_PT_AND_SY,
            *topics::PENDLE_SWAP_YT_AND_SY,
        ] {
            let log = make_log(
                address!("d0019e86edB35E1fedaaB03aED5c3c60f115d28b"), // router-ish emitter
                vec![
                    topic,
                    caller.into_word(),
                    market.into_word(),
                    receiver.into_word(),
                ],
                vec![0u8; 64],
            );
            let hit = classify_dex_event(&log).expect("pendle router swap must classify");
            assert_eq!(hit.dex_type, DexType::Pendle);
            assert!(hit.pool_id.is_none());
            assert!(hit.tokens.is_none());
            assert_eq!(hit.addr_override, Some(market));
        }
    }

    #[test]
    fn merge_from_dex_type_conflict_rules() {
        let t0 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let t1 = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        // Remote V2-fallback (fee unset) yields to a specific on-chain type.
        let mut fallback = DiscoveredPool::new(Address::ZERO, t0, t1, 0, DexType::UniswapV2, 0);
        let onchain_v3 = DiscoveredPool::new(Address::ZERO, t0, t1, 500, DexType::UniswapV3, 100);
        fallback.merge_from(&onchain_v3);
        assert_eq!(fallback.dex_type, DexType::UniswapV3);
        assert_eq!(fallback.fee, 500);

        // A specific type never yields to a bare V2 label.
        let mut v3 = DiscoveredPool::new(Address::ZERO, t0, t1, 3000, DexType::UniswapV3, 0);
        let remote_v2 = DiscoveredPool::new(Address::ZERO, t0, t1, 0, DexType::UniswapV2, 0);
        v3.merge_from(&remote_v2);
        assert_eq!(v3.dex_type, DexType::UniswapV3);

        // A confirmed on-chain V2 pool (explicit fee) is not flipped by a
        // conflicting remote label.
        let mut confirmed_v2 =
            DiscoveredPool::new(Address::ZERO, t0, t1, 30, DexType::UniswapV2, 0);
        let mislabeled_v3 = DiscoveredPool::new(Address::ZERO, t0, t1, 0, DexType::UniswapV3, 0);
        confirmed_v2.merge_from(&mislabeled_v3);
        assert_eq!(confirmed_v2.dex_type, DexType::UniswapV2);

        // Two conflicting specific types: first-seen wins.
        let mut curve = DiscoveredPool::new(Address::ZERO, t0, t1, 0, DexType::Curve, 0);
        let balancer = DiscoveredPool::new(Address::ZERO, t0, t1, 0, DexType::Balancer, 0);
        curve.merge_from(&balancer);
        assert_eq!(curve.dex_type, DexType::Curve);
    }

    fn all_factory_config<'a>() -> DiscoveryConfig<'a> {
        DiscoveryConfig {
            batch_size: 1000,
            factory_batch_size: 1000,
            v2_fee_override: Some(30),
            balancer_vault: Some(Address::ZERO),
            v2_factories: Some(&[]),
            v3_factories: Some(&[]),
            curve_registry: Some(Address::ZERO),
            curve_factories: Some(&[]),
            solidly_factories: Some(&[]),
            camelot_factories: Some(&[]),
            v4_pool_manager: Some(Address::ZERO),
            infinity_cl_pool_manager: Some(Address::ZERO),
            metric_factory: Some(Address::ZERO),
            fluid_factory: Some(Address::ZERO),
            solidly_fee_bps: None,
            trader_joe_factories: Some(&[]),
            pendle_factory: Some(Address::ZERO),
            rpc_concurrency: 1,
            token_cache: None,
            pool_cache: None,
        }
    }

    fn word(addr: Address) -> B256 {
        addr.into_word()
    }

    /// The unified decoder used by both `getLogs` and receipts must recognize
    /// every factory topic, so a receipt-derived log cannot silently lose a
    /// pool compared with the existing scanner.
    #[test]
    fn receipt_factory_decoder_covers_all_supported_dexes() {
        let factory = address!("1111111111111111111111111111111111111111");
        let token0 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let token1 = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let pool = address!("cccccccccccccccccccccccccccccccccccccccc");

        // Config gates each decoder by emitter. Use one synthetic factory for
        // every family so all branches are exercised with distinct topics.
        let v2_factories = [factory];
        let v3_factories = [factory];
        let curve_factories = [factory];
        let solidly_factories = [factory];
        let camelot_factories = [factory];
        let trader_joe_factories = [factory];
        let mut cfg = all_factory_config();
        cfg.balancer_vault = Some(factory);
        cfg.v2_factories = Some(&v2_factories);
        cfg.v3_factories = Some(&v3_factories);
        cfg.curve_registry = Some(factory);
        cfg.curve_factories = Some(&curve_factories);
        cfg.solidly_factories = Some(&solidly_factories);
        cfg.camelot_factories = Some(&camelot_factories);
        cfg.v4_pool_manager = Some(factory);
        cfg.infinity_cl_pool_manager = Some(factory);
        cfg.metric_factory = Some(factory);
        cfg.fluid_factory = Some(factory);
        cfg.trader_joe_factories = Some(&trader_joe_factories);
        cfg.pendle_factory = Some(factory);

        let mut fee_topic = B256::ZERO;
        fee_topic[28..32].copy_from_slice(&3000u32.to_be_bytes());
        let mut pool_type = B256::ZERO;
        pool_type[31] = 1;

        let cases: Vec<(alloy::rpc::types::Log, Address)> = vec![
            (
                make_log(
                    factory,
                    vec![*V2_PAIR_CREATED_TOPIC, word(token0), word(token1)],
                    [word(pool).as_slice(), &[0u8; 32][..]].concat(),
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*V3_POOL_CREATED_TOPIC, word(token0), word(token1), fee_topic],
                    {
                        let mut data = vec![0u8; 64];
                        data[28..32].copy_from_slice(&60i32.to_be_bytes());
                        data[32..64].copy_from_slice(word(pool).as_slice());
                        data
                    },
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*SOLIDLY_PAIR_CREATED_TOPIC, word(token0), word(token1)],
                    {
                        let mut data = vec![0u8; 64];
                        data[31] = 1;
                        data[32..64].copy_from_slice(word(pool).as_slice());
                        data
                    },
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*CAMELOT_PAIR_CREATED_TOPIC, word(token0), word(token1)],
                    {
                        let mut data = vec![0u8; 96];
                        data[0..32].copy_from_slice(word(pool).as_slice());
                        data[63] = 25;
                        data[95] = 1;
                        data
                    },
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*CURVE_POOL_ADDED_TOPIC, word(pool)],
                    vec![0u8; 32],
                ),
                pool,
            ),
            (
                make_log(factory, vec![*CURVE_POOL_DEPLOYED_TOPIC], word(pool).to_vec()),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![
                        *BALANCER_POOL_REGISTERED_TOPIC,
                        B256::repeat_byte(0x22),
                        word(pool),
                        pool_type,
                    ],
                    vec![0u8; 32],
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*LB_PAIR_CREATED_TOPIC, word(pool), word(token0), word(token1)],
                    vec![0u8; 64],
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*PENDLE_NEW_MARKET_TOPIC, word(pool), word(token0)],
                    vec![0u8; 32],
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*V4_INITIALIZE_TOPIC, word(pool), word(token0), word(token1)],
                    vec![0u8; 160],
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![
                        *INF_CL_INITIALIZE_TOPIC,
                        word(pool),
                        word(token0),
                        word(token1),
                    ],
                    vec![0u8; 160],
                ),
                pool,
            ),
            (
                make_log(
                    factory,
                    vec![*METRIC_POOL_CREATED_TOPIC, word(token0), word(token1), B256::ZERO],
                    [word(pool).as_slice(), &[0u8; 32][..]].concat(),
                ),
                pool,
            ),
            (
                make_log(factory, vec![*FLUID_DEX_DEPLOYED_TOPIC, word(pool)], vec![0u8; 32]),
                pool,
            ),
        ];

        for (idx, (log, expected)) in cases.into_iter().enumerate() {
            let (addr, decoded) = decode_factory_creation_log(&log, &cfg).unwrap_or_else(|| {
                panic!("case {idx}: missing decoder for topic {:?}", log.topics()[0])
            });
            assert_eq!(addr, expected);
            assert_eq!(decoded.creation_block, 1000);
            assert_eq!(decoded.factory, Some(factory));
        }
    }

    #[test]
    fn receipt_log_conversion_keeps_block_and_log_index() {
        let log = crate::data::types::LogData {
            log_index: Some(7),
            address: address!("dddddddddddddddddddddddddddddddddddddddd"),
            topics: vec![*V2_PAIR_CREATED_TOPIC],
            data: Bytes::from_static(&[0u8; 32]),
        };
        let rpc_log = receipt_log_to_rpc_log(&log, 12345, 3, 99);
        assert_eq!(rpc_log.block_number, Some(12345));
        assert_eq!(rpc_log.log_index, Some(7));
        assert_eq!(rpc_log.transaction_index, Some(3));
        assert_eq!(rpc_log.address(), log.address);
    }
}
