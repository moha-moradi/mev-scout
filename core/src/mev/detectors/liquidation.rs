use std::collections::{HashMap, HashSet};
use alloy::primitives::{keccak256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::types::MevOpportunity;
use crate::pool::state::{calldata_gas_estimate, PoolManager};
use crate::rpc::RpcClient;
use crate::types::{GasConfig, Strategy};

/// Aave V3 LiquidationCall event signature.
static LIQUIDATION_CALL_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)"));

/// Aave V3 Supply event signature.
static SUPPLY_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Supply(address,address,address,uint256,uint16)"));

/// Aave V3 Borrow event signature.
static BORROW_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Borrow(address,address,address,uint256,uint8,uint256,uint16)"));

/// Aave V3 Withdraw event signature.
static WITHDRAW_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Withdraw(address,address,address,uint256)"));

/// Aave V3 Repay event signature.
static REPAY_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("Repay(address,address,address,uint256,bool)"));

/// Default fallback constants when on-chain reserve data is unavailable.
const FALLBACK_LIQUIDATION_THRESHOLD_BPS: u16 = 8000; // 80.00%
const FALLBACK_LIQUIDATION_BONUS_BPS: u16 = 500; // 5.00%
#[allow(dead_code)]
const FALLBACK_LTV_BPS: u16 = 7500; // 75.00%
const MAX_CLOSE_FACTOR_NUM: u128 = 50; // 50%
const MAX_CLOSE_FACTOR_DEN: u128 = 100;
const LIQUIDATION_GAS_LIMIT: u64 = 180_000;

/// Per-asset reserve parameters fetched from the Aave V3 Pool contract.
/// Used to replace hardcoded constants with real protocol data during proactive detection.
#[derive(Debug, Clone, Copy)]
pub struct AaveReserveData {
    pub liquidation_threshold_bps: u16,
    pub liquidation_bonus_bps: u16,
    pub ltv_bps: u16,
}

/// Cache of Aave V3 reserve data keyed by asset address.
/// Populated via on-chain `eth_call` to `getReserveData()` before detection runs.
/// When empty or missing a token, `detect()` falls back to hardcoded defaults.
#[derive(Debug, Clone, Default)]
pub struct AaveReserveCache {
    reserves: HashMap<Address, AaveReserveData>,
}

impl AaveReserveCache {
    /// Fetch reserve data for a single asset from the Aave V3 Pool contract.
    /// Calls `getReserveData(address)` (selector: 0x35ea6a75) via `eth_call`
    /// and decodes the `configuration` bitmap to extract LTV, liquidation
    /// threshold, and liquidation bonus.
    pub async fn fetch_reserve(
        rpc: &RpcClient,
        aave_pool: Address,
        token: Address,
        block: u64,
    ) -> Option<AaveReserveData> {
        let selector = [0x35, 0xea, 0x6a, 0x75];
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&selector);
        let mut token_bytes = [0u8; 32];
        token_bytes[12..32].copy_from_slice(token.as_slice());
        calldata.extend_from_slice(&token_bytes);

        let result = rpc.call(aave_pool, calldata.into(), block).await.ok()?;
        if result.len() < 64 {
            return None;
        }

        // configuration is the first uint256 (32 bytes) of ReserveData struct
        let config = U256::from_be_slice(&result[..32]);

        Some(AaveReserveData {
            ltv_bps: (config & U256::from(0xFFFFu64)).to::<u16>(),
            liquidation_threshold_bps: ((config >> U256::from(16u64)) & U256::from(0xFFFFu64)).to::<u16>(),
            liquidation_bonus_bps: ((config >> U256::from(32u64)) & U256::from(0xFFFFu64)).to::<u16>(),
        })
    }

    pub fn get(&self, token: &Address) -> Option<&AaveReserveData> {
        self.reserves.get(token)
    }

    pub fn insert(&mut self, token: Address, data: AaveReserveData) {
        self.reserves.insert(token, data);
    }

    pub fn is_empty(&self) -> bool {
        self.reserves.is_empty()
    }

    pub fn len(&self) -> usize {
        self.reserves.len()
    }

    /// Pre-fetch reserve data for a set of tokens at the given block.
    /// Skips tokens already in the cache. Tokens that fail to fetch
    /// are silently skipped (the caller will fall back to defaults).
    pub async fn prefetch(
        &mut self,
        rpc: &RpcClient,
        aave_pool: Address,
        tokens: &[Address],
        block: u64,
    ) {
        for &token in tokens {
            if self.reserves.contains_key(&token) {
                continue;
            }
            if let Some(data) = Self::fetch_reserve(rpc, aave_pool, token, block).await {
                self.reserves.insert(token, data);
            }
        }
    }
}

/// Standard Aave V3 health factor formula:
/// `HF = (totalCollateral * avgLiquidationThreshold) / totalDebt`
///
/// Returns `f64::MAX` when `total_debt_native` is zero (position is healthy).
/// A health factor below 1.0 means the position can be liquidated.
/// Values between 1.0 and 1.1 are approaching liquidation.
pub fn compute_health_factor(
    total_collateral_native: u128,
    total_debt_native: u128,
    avg_liquidation_threshold_bps: u16,
) -> f64 {
    if total_debt_native == 0 {
        return f64::MAX;
    }
    let threshold = avg_liquidation_threshold_bps as f64 / 10000.0;
    (total_collateral_native as f64 * threshold) / total_debt_native as f64
}

#[derive(Debug, Clone)]
struct LiquidationEvent {
    tx_index: usize,
    collateral_asset: Address,
    debt_asset: Address,
    #[allow(dead_code)]
    user: Address,
    debt_to_cover: u128,
    liquidated_collateral_amount: u128,
}

/// Tracked position of a single user across all Aave V3 assets.
#[derive(Debug, Clone, Default)]
struct UserPosition {
    collateral: HashMap<Address, u128>,
    debt: HashMap<Address, u128>,
}

/// Detects Aave V3 liquidation events during block replay.
///
/// Two detection modes:
/// 1. **Reactive** — captures on-chain `LiquidationCall` events (existing behaviour).
/// 2. **Proactive** — tracks `Supply`/`Borrow`/`Withdraw`/`Repay` events to build user
///    positions in memory, then scans for a health factor < 1 and emits opportunities
///    for underwater positions regardless of whether a liquidation was actually executed.
pub struct LiquidationDetector {
    block_number: u64,
    current_tx_index: usize,
    events: Vec<LiquidationEvent>,
    emitted: HashSet<(Address, Address, Address)>,
    users: HashMap<Address, UserPosition>,
    reserve_cache: AaveReserveCache,
}

impl LiquidationDetector {
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            current_tx_index: 0,
            events: Vec::new(),
            emitted: HashSet::new(),
            users: HashMap::new(),
            reserve_cache: AaveReserveCache::default(),
        }
    }

    /// Attach pre-fetched Aave V3 reserve data for per-asset liquidation parameters.
    /// When set, `detect()` uses real on-chain thresholds and bonuses instead of
    /// hardcoded 80%/5% defaults. Call `AaveReserveCache::prefetch()` to populate.
    pub fn with_reserve_cache(mut self, cache: AaveReserveCache) -> Self {
        self.reserve_cache = cache;
        self
    }

    /// Process a transaction's logs, extracting Aave V3 events.
    ///
    /// - `LiquidationCall` → stored for reactive emission AND user position is updated.
    /// - `Supply`/`Borrow`/`Withdraw`/`Repay` → user position tracking for proactive detection.
    pub fn process_tx(&mut self, tx_index: usize, logs: &[ExecutedLog]) {
        self.current_tx_index = tx_index;
        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            let sig = log.topics[0];

            if sig == *LIQUIDATION_CALL_TOPIC {
                Self::process_liquidation_call(self, log, tx_index);
            } else if sig == *SUPPLY_TOPIC {
                Self::process_supply(self, log);
            } else if sig == *BORROW_TOPIC {
                Self::process_borrow(self, log);
            } else if sig == *WITHDRAW_TOPIC {
                Self::process_withdraw(self, log);
            } else if sig == *REPAY_TOPIC {
                Self::process_repay(self, log);
            }
        }
    }

    // ── Event decoders ────────────────────────────────────────────

    /// Decode a `LiquidationCall` event and optionally record it.
    ///
    /// Event: LiquidationCall(address indexed collateralAsset, address indexed debtAsset,
    ///        address indexed user, uint256 debtToCover, uint256 liquidatedCollateralAmount,
    ///        address liquidator, bool receiveAToken)
    ///
    /// Topics: [sig, collateralAsset, debtAsset, user]
    /// Data:   [debtToCover(32), liquidatedCollateral(32), liquidator(32), receiveAToken(32)]
    fn process_liquidation_call(&mut self, log: &ExecutedLog, tx_index: usize) {
        if log.topics.len() < 4 || log.data.len() < 128 {
            return;
        }
        let collateral_asset = Address::from_slice(&log.topics[1][12..32]);
        let debt_asset = Address::from_slice(&log.topics[2][12..32]);
        let user = Address::from_slice(&log.topics[3][12..32]);
        let debt_to_cover = U256::from_be_slice(&log.data[..32]).to::<u128>();
        let liquidated_collateral = U256::from_be_slice(&log.data[32..64]).to::<u128>();
        if debt_to_cover == 0 || liquidated_collateral == 0 {
            return;
        }

        // Update tracked position (the user's debt & collateral decreased)
        if let Some(pos) = self.users.get_mut(&user) {
            decrease_balance(&mut pos.collateral, collateral_asset, liquidated_collateral);
            decrease_balance(&mut pos.debt, debt_asset, debt_to_cover);
        }

        // Store for reactive emission (dedup via emitted set)
        let key = (collateral_asset, debt_asset, user);
        if self.emitted.insert(key) {
            self.events.push(LiquidationEvent {
                tx_index,
                collateral_asset,
                debt_asset,
                user,
                debt_to_cover,
                liquidated_collateral_amount: liquidated_collateral,
            });
        }
    }

    /// Supply(address indexed reserve, address indexed user, address indexed onBehalfOf, uint256 amount, uint16 referralCode)
    /// Topics: [sig, reserve, user, onBehalfOf]
    /// Data:   [amount(32), referralCode(32)]
    fn process_supply(&mut self, log: &ExecutedLog) {
        if log.topics.len() < 4 || log.data.len() < 32 {
            return;
        }
        let reserve = Address::from_slice(&log.topics[1][12..32]);
        let on_behalf = Address::from_slice(&log.topics[3][12..32]);
        let amount = U256::from_be_slice(&log.data[..32]).to::<u128>();
        if amount == 0 {
            return;
        }
        let pos = self.users.entry(on_behalf).or_default();
        *pos.collateral.entry(reserve).or_insert(0) = pos.collateral.get(&reserve).copied().unwrap_or(0).saturating_add(amount);
    }

    /// Borrow(address indexed reserve, address indexed user, address indexed onBehalfOf,
    ///       uint256 amount, uint8 interestRateMode, uint256 borrowRate, uint16 referralCode)
    /// Topics: [sig, reserve, user, onBehalfOf]
    /// Data:   [amount(32), interestRateMode(32), borrowRate(32), referralCode(32)]
    fn process_borrow(&mut self, log: &ExecutedLog) {
        if log.topics.len() < 4 || log.data.len() < 32 {
            return;
        }
        let reserve = Address::from_slice(&log.topics[1][12..32]);
        let on_behalf = Address::from_slice(&log.topics[3][12..32]);
        let amount = U256::from_be_slice(&log.data[..32]).to::<u128>();
        if amount == 0 {
            return;
        }
        let pos = self.users.entry(on_behalf).or_default();
        *pos.debt.entry(reserve).or_insert(0) = pos.debt.get(&reserve).copied().unwrap_or(0).saturating_add(amount);
    }

    /// Withdraw(address indexed reserve, address indexed user, address indexed to, uint256 amount)
    /// Topics: [sig, reserve, user, to]
    /// Data:   [amount(32)]
    fn process_withdraw(&mut self, log: &ExecutedLog) {
        if log.topics.len() < 3 || log.data.len() < 32 {
            return;
        }
        let reserve = Address::from_slice(&log.topics[1][12..32]);
        let user = Address::from_slice(&log.topics[2][12..32]);
        let amount = U256::from_be_slice(&log.data[..32]).to::<u128>();
        if amount == 0 {
            return;
        }
        if let Some(pos) = self.users.get_mut(&user) {
            decrease_balance(&mut pos.collateral, reserve, amount);
        }
    }

    /// Repay(address indexed reserve, address indexed user, address indexed onBehalfOf, uint256 amount, bool useATokens)
    /// Topics: [sig, reserve, user, onBehalfOf]
    /// Data:   [amount(32), useATokens(32)]
    fn process_repay(&mut self, log: &ExecutedLog) {
        if log.topics.len() < 4 || log.data.len() < 32 {
            return;
        }
        let reserve = Address::from_slice(&log.topics[1][12..32]);
        let on_behalf = Address::from_slice(&log.topics[3][12..32]);
        let amount = U256::from_be_slice(&log.data[..32]).to::<u128>();
        if amount == 0 {
            return;
        }
        if let Some(pos) = self.users.get_mut(&on_behalf) {
            decrease_balance(&mut pos.debt, reserve, amount);
        }
    }

    /// Emit liquidation opportunities from collected events (reactive) and
    /// scanned user positions (proactive).
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let events = std::mem::take(&mut self.events);

        // 1. Reactive: emit stored LiquidationCall events
        for ev in &events {
            if let Some(opp) = self.emit_opportunity(ev, pool_manager, timestamp, base_fee_per_gas, gas_config) {
                opportunities.push(opp);
            }
        }

        // 2. Proactive: scan tracked users for health factor < 1
        for (&user, pos) in &self.users.clone() {
            if pos.debt.is_empty() {
                continue;
            }

            // Compute total collateral and debt in native ETH,
            // plus weighted average liquidation threshold from per-asset data.
            let mut total_collateral_native = 0u128;
            let mut total_debt_native = 0u128;
            let mut weighted_threshold_sum = 0u128; // collateral_native_i * threshold_bps_i

            for (&asset, &amount) in &pos.collateral {
                if let Some(native) = pool_manager.normalize_to_native(asset, amount) {
                    let threshold_bps = self.reserve_cache
                        .get(&asset)
                        .map(|d| d.liquidation_threshold_bps as u128)
                        .unwrap_or(FALLBACK_LIQUIDATION_THRESHOLD_BPS as u128);
                    total_collateral_native = total_collateral_native.saturating_add(native);
                    weighted_threshold_sum = weighted_threshold_sum.saturating_add(native.saturating_mul(threshold_bps));
                }
            }

            for (&asset, &amount) in &pos.debt {
                if let Some(native) = pool_manager.normalize_to_native(asset, amount) {
                    total_debt_native = total_debt_native.saturating_add(native);
                }
            }

            if total_debt_native == 0 {
                continue;
            }

            // Compute weighted average liquidation threshold
            let avg_threshold_bps = if total_collateral_native > 0 {
                (weighted_threshold_sum / total_collateral_native) as u16
            } else {
                FALLBACK_LIQUIDATION_THRESHOLD_BPS
            };

            // Compute health factor using the real Aave V3 formula
            let hf = compute_health_factor(total_collateral_native, total_debt_native, avg_threshold_bps);

            // Flag positions with HF < 1.0 (immediately liquidatable)
            // or HF < 1.1 (approaching liquidation, early warning)
            if hf >= 1.0 {
                continue;
            }

            // Pick the most valuable debt asset to close
            let best_debt = pos.debt.iter()
                .filter_map(|(&asset, &amount)| {
                    pool_manager.normalize_to_native(asset, amount)
                        .map(|val| (asset, amount, val))
                })
                .max_by_key(|&(_, _, val)| val);

            // Pick the most valuable collateral asset to seize
            let best_collateral = pos.collateral.iter()
                .filter_map(|(&asset, &amount)| {
                    pool_manager.normalize_to_native(asset, amount)
                        .map(|val| (asset, amount, val))
                })
                .max_by_key(|&(_, _, val)| val);

            let (debt_asset, total_debt_amount, best_debt_native) = match best_debt {
                Some(t) => t,
                None => continue,
            };
            let (collateral_asset, _total_collateral_amount, _best_collateral_native) = match best_collateral {
                Some(t) => t,
                None => continue,
            };

            // Dedup: same key as reactive events
            let key = (collateral_asset, debt_asset, user);
            if !self.emitted.insert(key) {
                continue;
            }

            // Close up to 50% of the user's debt
            let debt_to_cover = total_debt_amount
                .saturating_mul(MAX_CLOSE_FACTOR_NUM)
                .saturating_div(MAX_CLOSE_FACTOR_DEN);

            let debt_to_cover_native = best_debt_native
                .saturating_mul(MAX_CLOSE_FACTOR_NUM)
                .saturating_div(MAX_CLOSE_FACTOR_DEN);

            if debt_to_cover == 0 {
                continue;
            }

            // Use per-asset liquidation bonus if available, fall back to default 5%
            let bonus_bps = self.reserve_cache
                .get(&collateral_asset)
                .map(|d| d.liquidation_bonus_bps as u128)
                .unwrap_or(FALLBACK_LIQUIDATION_BONUS_BPS as u128);
            let profit_native = debt_to_cover_native
                .saturating_mul(bonus_bps)
                .saturating_div(10000);

            let gas_limit = LIQUIDATION_GAS_LIMIT.saturating_add(calldata_gas_estimate(2));
            let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

            // H9: Compute slippage — profit scales linearly with debt_to_cover (fixed bonus rate)
            let liq_slippage = |pct: u128| -> Option<U256> {
                let debt_adj = debt_to_cover.saturating_mul(pct) / 100;
                if debt_adj == 0 { return None; }
                let debt_native_adj = pool_manager
                    .normalize_to_native(debt_asset, debt_adj)
                    .unwrap_or(debt_adj);
                let profit_adj = debt_native_adj
                    .saturating_mul(bonus_bps as u128)
                    .saturating_div(10000);
                Some(U256::from(profit_adj))
            };
            opportunities.push(MevOpportunity {
                canonical_id: None,
                block_number: self.block_number,
                tx_index: self.current_tx_index,
                strategy: Strategy::Liquidation,
                pool_a: collateral_asset,
                pool_b: debt_asset,
                token_in: debt_asset,
                token_out: collateral_asset,
                input_amount: U256::from(debt_to_cover),
                expected_profit: U256::from(profit_native),
                raw_profit: Some(U256::from(debt_to_cover_native.saturating_add(profit_native))),
                profit_slippage_p1: liq_slippage(101),
                profit_slippage_m1: liq_slippage(99),
                profit_slippage_p2: liq_slippage(102),
                profit_slippage_m2: liq_slippage(98),
                gas_cost_wei,
                timestamp,
                path: None,
                tick_lower: None,
                tick_upper: None,
                liquidity_amount: None,
                victim_tx_index: None,
                backrun_tx_index: None,
                mempool_only: false,
                confidence: None,
            });
        }

        opportunities
    }

    fn emit_opportunity(
        &self,
        ev: &LiquidationEvent,
        pool_manager: &PoolManager,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Option<MevOpportunity> {
        let gas_limit = LIQUIDATION_GAS_LIMIT.saturating_add(calldata_gas_estimate(2));
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        let collateral_native = pool_manager.normalize_to_native(ev.collateral_asset, ev.liquidated_collateral_amount)
            .unwrap_or(ev.liquidated_collateral_amount);
        let debt_native = pool_manager.normalize_to_native(ev.debt_asset, ev.debt_to_cover)
            .unwrap_or(ev.debt_to_cover);

        let profit_native = collateral_native.saturating_sub(debt_native);

        // H9: Compute slippage — profit scales linearly with debt_to_cover
        let liq_slippage = |pct: u128| -> Option<U256> {
            let debt_adj = ev.debt_to_cover.saturating_mul(pct) / 100;
            if debt_adj == 0 { return None; }
            let debt_native_adj = pool_manager
                .normalize_to_native(ev.debt_asset, debt_adj)
                .unwrap_or(debt_adj);
            let ratio_adj = debt_native_adj * 1_000_000 / debt_native.max(1);
            Some(U256::from(profit_native.saturating_mul(ratio_adj) / 1_000_000))
        };
        Some(MevOpportunity {
            canonical_id: None,
            block_number: self.block_number,
            tx_index: ev.tx_index,
            strategy: Strategy::Liquidation,
            pool_a: ev.collateral_asset,
            pool_b: ev.debt_asset,
            token_in: ev.debt_asset,
            token_out: ev.collateral_asset,
            input_amount: U256::from(ev.debt_to_cover),
            expected_profit: U256::from(profit_native),
            raw_profit: None,
            profit_slippage_p1: liq_slippage(101),
            profit_slippage_m1: liq_slippage(99),
            profit_slippage_p2: liq_slippage(102),
            profit_slippage_m2: liq_slippage(98),
            gas_cost_wei,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        })
    }
}

/// Decrease a balance by `amount`, removing the entry if it would reach zero.
fn decrease_balance(map: &mut HashMap<Address, u128>, key: Address, amount: u128) {
    match map.get(&key) {
        Some(&cur) if cur <= amount => {
            map.remove(&key);
        }
        Some(&cur) => {
            map.insert(key, cur - amount);
        }
        None => {}
    }
}

