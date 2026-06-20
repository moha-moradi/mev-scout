use std::collections::HashSet;
use alloy::primitives::{keccak256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::mev::opportunity::MevOpportunity;
use crate::pool::state::{calldata_gas_estimate, PoolManager};
use crate::types::{GasConfig, Strategy};

/// Aave V3 LiquidationCall event signature.
static LIQUIDATION_CALL_TOPIC: std::sync::LazyLock<B256> =
    std::sync::LazyLock::new(|| keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)"));

#[derive(Debug, Clone)]
struct LiquidationEvent {
    tx_index: usize,
    collateral_asset: Address,
    debt_asset: Address,
    user: Address,
    debt_to_cover: u128,
    liquidated_collateral_amount: u128,
}

/// Detects Aave V3 liquidation events during block replay.
///
/// When a user's health factor drops below 1, anyone can repay their debt
/// in exchange for collateral at a discount (~5% liquidation bonus).
/// This detector captures those events and estimates the liquidator's profit.
pub struct LiquidationDetector {
    block_number: u64,
    events: Vec<LiquidationEvent>,
    emitted: HashSet<(Address, Address, Address)>,
}

impl LiquidationDetector {
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            events: Vec::new(),
            emitted: HashSet::new(),
        }
    }

    /// Process a transaction's logs, extracting Aave V3 LiquidationCall events.
    pub fn process_tx(&mut self, tx_index: usize, logs: &[ExecutedLog]) {
        for log in logs {
            if log.topics.is_empty() || log.topics[0] != *LIQUIDATION_CALL_TOPIC {
                continue;
            }
            if let Some(liq) = Self::decode_liquidation_call(log, tx_index) {
                let key = (liq.collateral_asset, liq.debt_asset, liq.user);
                if self.emitted.insert(key) {
                    self.events.push(liq);
                }
            }
        }
    }

    /// Decode an Aave V3 LiquidationCall event from log data.
    ///
    /// Event: LiquidationCall(address indexed collateralAsset, address indexed debtAsset,
    ///        address indexed user, uint256 liquidationAmount, uint256 liquidatedCollateralAmount,
    ///        address liquidator, bool receiveAToken)
    ///
    /// Topics: [sig, collateralAsset, debtAsset, user]
    /// Data:   [debtToCover(32), liquidatedCollateral(32), liquidator(32), receiveAToken(32)]
    fn decode_liquidation_call(log: &ExecutedLog, tx_index: usize) -> Option<LiquidationEvent> {
        if log.topics.len() < 4 || log.data.len() < 128 {
            return None;
        }

        let collateral_asset = Address::from_slice(&log.topics[1][12..32]);
        let debt_asset = Address::from_slice(&log.topics[2][12..32]);
        let user = Address::from_slice(&log.topics[3][12..32]);

        let debt_to_cover = U256::from_be_slice(&log.data[..32]).to::<u128>();
        let liquidated_collateral_amount = U256::from_be_slice(&log.data[32..64]).to::<u128>();

        if debt_to_cover == 0 || liquidated_collateral_amount == 0 {
            return None;
        }

        Some(LiquidationEvent {
            tx_index,
            collateral_asset,
            debt_asset,
            user,
            debt_to_cover,
            liquidated_collateral_amount,
        })
    }

    /// Emit liquidation opportunities from events collected during `process_tx`.
    ///
    /// Profit is estimated as `liquidated_collateral - debt_to_cover` normalized to
    /// native token. Liquidators receive collateral at a discount, so gross profit
    /// is the liquidation bonus amount.
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let events = std::mem::take(&mut self.events);

        for ev in &events {
            let opp = self.emit_opportunity(ev, pool_manager, timestamp, base_fee_per_gas, gas_config);
            if let Some(opp) = opp {
                opportunities.push(opp);
            }
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
        // H7: Aave V3 liquidation costs ~180k gas empirical. Add calldata overhead.
        let gas_limit = 180_000 + calldata_gas_estimate(2);
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        // Gross profit = collateral received - debt repaid.
        // Normalize both to native before subtracting (C5).
        let collateral_native = pool_manager.normalize_to_native(ev.collateral_asset, ev.liquidated_collateral_amount)
            .unwrap_or(ev.liquidated_collateral_amount);
        let debt_native = pool_manager.normalize_to_native(ev.debt_asset, ev.debt_to_cover)
            .unwrap_or(ev.debt_to_cover);

        let profit_native = collateral_native.saturating_sub(debt_native);

        Some(MevOpportunity {
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
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn addr_to_topic(a: Address) -> B256 {
        let mut b = [0u8; 32];
        b[12..32].copy_from_slice(a.as_slice());
        B256::from(b)
    }

    fn sample_log(
        topics: Vec<B256>,
        data_len: usize,
        debt: u128,
        collateral: u128,
    ) -> ExecutedLog {
        let mut data = vec![0u8; data_len];
        if data_len >= 64 {
            let mut val = [0u8; 32];
            val[16..32].copy_from_slice(&debt.to_be_bytes());
            data[..32].copy_from_slice(&val);
            let mut val2 = [0u8; 32];
            val2[16..32].copy_from_slice(&collateral.to_be_bytes());
            data[32..64].copy_from_slice(&val2);
        }
        ExecutedLog {
            address: address!("794a61358D6845594F94dc1DB02A252b5b4814aD"),
            topics,
            data: data.into(),
        }
    }

    #[test]
    fn test_liquidation_call_topic_hash() {
        let expected = keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)");
        assert_eq!(*LIQUIDATION_CALL_TOPIC, expected);
    }

    #[test]
    fn test_decode_valid_liquidation() {
        let collateral = address!("c2132D05D31c914a87C6611C10748AEb04B58e8F");
        let debt = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
        let user = address!("1111111111111111111111111111111111111111");

        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, addr_to_topic(collateral), addr_to_topic(debt), addr_to_topic(user)],
            128, 1000, 1200,
        );
        let decoded = LiquidationDetector::decode_liquidation_call(&log, 5);
        assert!(decoded.is_some());
        let liq = decoded.unwrap();
        assert_eq!(liq.tx_index, 5);
        assert_eq!(liq.collateral_asset, collateral);
        assert_eq!(liq.debt_asset, debt);
        assert_eq!(liq.user, user);
        assert_eq!(liq.debt_to_cover, 1000);
        assert_eq!(liq.liquidated_collateral_amount, 1200);
    }

    #[test]
    fn test_decode_wrong_topic_skipped() {
        let log = sample_log(vec![B256::ZERO], 128, 1000, 1200);
        assert!(LiquidationDetector::decode_liquidation_call(&log, 0).is_none());
    }

    #[test]
    fn test_decode_short_data_returns_none() {
        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            64, 1000, 1200,
        );
        assert!(LiquidationDetector::decode_liquidation_call(&log, 0).is_none());
    }

    #[test]
    fn test_decode_zero_amounts_skipped() {
        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            128, 0, 0,
        );
        assert!(LiquidationDetector::decode_liquidation_call(&log, 0).is_none());
    }

    #[test]
    fn test_process_tx_adds_event() {
        let collateral = address!("c2132D05D31c914a87C6611C10748AEb04B58e8F");
        let debt = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
        let user = address!("1111111111111111111111111111111111111111");

        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, addr_to_topic(collateral), addr_to_topic(debt), addr_to_topic(user)],
            128, 1000, 1200,
        );

        let mut detector = LiquidationDetector::new(1);
        detector.process_tx(3, &[log]);
        assert_eq!(detector.events.len(), 1);
        assert_eq!(detector.emitted.len(), 1);
    }

    #[test]
    fn test_process_tx_dedup_same_liquidation() {
        let collateral = address!("c2132D05D31c914a87C6611C10748AEb04B58e8F");
        let debt = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
        let user = address!("1111111111111111111111111111111111111111");

        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, addr_to_topic(collateral), addr_to_topic(debt), addr_to_topic(user)],
            128, 1000, 1200,
        );

        let mut detector = LiquidationDetector::new(1);
        detector.process_tx(3, &[log.clone()]);
        detector.process_tx(5, &[log]);
        assert_eq!(detector.events.len(), 1);
        assert_eq!(detector.emitted.len(), 1);
    }

    #[test]
    fn test_detect_clears_events() {
        let collateral = address!("c2132D05D31c914a87C6611C10748AEb04B58e8F");
        let debt = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
        let user = address!("1111111111111111111111111111111111111111");

        let log = sample_log(
            vec![*LIQUIDATION_CALL_TOPIC, addr_to_topic(collateral), addr_to_topic(debt), addr_to_topic(user)],
            128, 1000, 1200,
        );

        let mut detector = LiquidationDetector::new(1);
        detector.process_tx(3, &[log]);

        let pm = PoolManager::default();
        let gas = GasConfig::default();
        let opps = detector.detect(&pm, 100, 50_000_000_000, gas);

        // Events should be cleared after detect
        assert!(detector.events.is_empty());
        // Should produce at least one opportunity
        assert!(!opps.is_empty());
        assert_eq!(opps[0].strategy, Strategy::Liquidation);
        assert_eq!(opps[0].input_amount, U256::from(1000u128));
        assert_eq!(opps[0].token_in, debt);
        assert_eq!(opps[0].token_out, collateral);
    }
}
