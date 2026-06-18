//! Data contracts for detected MEV opportunities and persisted result files.
//!
//! These types are the serialization boundary between the core backtest engine,
//! the CLI output layer, and the API serialization layer.

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};
use crate::types::Strategy;

/// A detected MEV opportunity from backtesting.
///
/// Different strategies populate different optional fields:
/// - `path` for multi-hop strategies,
/// - `tick_lower`/`tick_upper`/`liquidity_amount` for JIT strategies,
/// - `victim_tx_index`/`backrun_tx_index` for sandwich attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpportunity {
    /// Block where the opportunity was detected
    /// Block where the opportunity was detected
    pub block_number: u64,
    /// Index of the transaction after which the opportunity exists
    pub tx_index: usize,
    /// The strategy type
    pub strategy: Strategy,
    /// Pool involved in the first swap
    pub pool_a: Address,
    /// Pool involved in the second swap
    pub pool_b: Address,
    /// Token being arbitraged (input token)
    pub token_in: Address,
    /// Token received as output
    pub token_out: Address,
    /// Amount of token_in to invest
    pub input_amount: U256,
    /// Expected profit in token_out (gross, before gas)
    pub expected_profit: U256,
    /// Raw profit in token_out before normalization to native (None = same as expected_profit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_profit: Option<U256>,
    /// Profit estimate with +1% slippage (more input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p1: Option<U256>,
    /// Profit estimate with -1% slippage (less input) — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m1: Option<U256>,
    /// Profit estimate with +2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_p2: Option<U256>,
    /// Profit estimate with -2% slippage — None if not computed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit_slippage_m2: Option<U256>,
    /// Estimated gas cost in wei
    pub gas_cost_wei: u128,
    /// Timestamp of the block
    pub timestamp: u64,
    /// Full pool path for multi-hop opportunities (e.g., [buy, intermediate, ..., sell])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<Address>>,
    /// Tick range lower bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_lower: Option<i32>,
    /// Tick range upper bound (JIT liquidity positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_upper: Option<i32>,
    /// Amount of liquidity deployed (JIT positions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquidity_amount: Option<u128>,
    /// Transaction index of the victim's swap (sandwich attacks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victim_tx_index: Option<usize>,
    /// Transaction index of the backrun (sandwich attacks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backrun_tx_index: Option<usize>,
}

impl MevOpportunity {
    /// Create a new MEV opportunity with required fields.
    /// Strategy-specific fields should be set via builder methods.
    pub fn new(
        block_number: u64,
        tx_index: usize,
        strategy: Strategy,
        pool_a: Address,
        timestamp: u64,
    ) -> Self {
        MevOpportunity {
            block_number,
            tx_index,
            strategy,
            pool_a,
            pool_b: Address::ZERO,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            input_amount: U256::ZERO,
            expected_profit: U256::ZERO,
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 0,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        }
    }

    /// Set JIT-specific fields: tick range and liquidity amount.
    /// Panics in debug builds if strategy is not JIT or JitArb.
    pub fn with_jit_fields(mut self, tick_lower: i32, tick_upper: i32, liquidity: u128) -> Self {
        debug_assert!(
            self.strategy == Strategy::Jit || self.strategy == Strategy::JitArb,
            "JIT fields only valid for Jit/JitArb strategies"
        );
        self.tick_lower = Some(tick_lower);
        self.tick_upper = Some(tick_upper);
        self.liquidity_amount = Some(liquidity);
        self
    }

    /// Set sandwich-specific fields: victim and backrun tx indices.
    /// Panics in debug builds if strategy is not Sandwich.
    pub fn with_sandwich_fields(mut self, victim_tx_index: usize, backrun_tx_index: usize) -> Self {
        debug_assert!(
            self.strategy == Strategy::Sandwich,
            "Sandwich fields only valid for Sandwich strategy"
        );
        self.victim_tx_index = Some(victim_tx_index);
        self.backrun_tx_index = Some(backrun_tx_index);
        self
    }

    /// Set multi-hop path.
    /// Panics in debug builds if strategy is not MultiHopArb.
    pub fn with_path(mut self, path: Vec<Address>) -> Self {
        debug_assert!(
            self.strategy == Strategy::MultiHopArb,
            "Path only valid for MultiHopArb strategy"
        );
        self.path = Some(path);
        self
    }
}

/// Saved results file wrapping opportunities with run metadata.
///
/// Written to `export_path` and re-read by the `report` subcommand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsFile {
    pub run_id: String,
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub range_mode: String,
    pub strategies: Vec<String>,
    pub flash_loan_provider: String,
    pub resolved_at: u64,
    pub created_at: u64,
    pub opportunities: Vec<MevOpportunity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    #[test]
    fn test_mev_opportunity_path_roundtrip() {
        use alloy::primitives::address;
        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::MultiHopArb,
            pool_a: address!("1111111111111111111111111111111111111111"),
            pool_b: address!("3333333333333333333333333333333333333333"),
            token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            input_amount: U256::from(1000u64),
            expected_profit: U256::from(100u64),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 1_000_000,
            timestamp: 12345,
            path: Some(vec![
                address!("1111111111111111111111111111111111111111"),
                address!("2222222222222222222222222222222222222222"),
                address!("3333333333333333333333333333333333333333"),
            ]),
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
        };
        let json = serde_json::to_string(&opp).unwrap();
        let deserialized: MevOpportunity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.path, opp.path);
        assert!(json.contains("\"path\""));
    }

    #[test]
    fn test_mev_opportunity_jit_fields_roundtrip() {
        use alloy::primitives::address;
        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 5,
            strategy: Strategy::Jit,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(0),
            expected_profit: U256::from(1000),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 0,
            timestamp: 12345,
            path: None,
            tick_lower: Some(-88720),
            tick_upper: Some(88720),
            liquidity_amount: Some(500_000u128),
            victim_tx_index: None,
            backrun_tx_index: None,
        };
        let json = serde_json::to_string(&opp).unwrap();
        let deserialized: MevOpportunity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tick_lower, Some(-88720));
        assert_eq!(deserialized.tick_upper, Some(88720));
        assert_eq!(deserialized.liquidity_amount, Some(500_000));
        assert!(json.contains("\"tick_lower\""));
        assert!(json.contains("\"tick_upper\""));
        assert!(json.contains("\"liquidity_amount\""));

        // Verify JIT fields are absent from serde output when None
        let no_jit = MevOpportunity {
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            ..opp
        };
        let json_no = serde_json::to_string(&no_jit).unwrap();
        assert!(!json_no.contains("tick_lower"));
        assert!(!json_no.contains("tick_upper"));
        assert!(!json_no.contains("liquidity_amount"));
    }

    #[test]
    fn test_jit_opportunity_must_have_tick_fields() {
        use alloy::primitives::address;
        let opp = MevOpportunity::new(1, 0, Strategy::Jit, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
            .with_jit_fields(-88720, 88720, 500_000);
        assert_eq!(opp.tick_lower, Some(-88720));
        assert_eq!(opp.tick_upper, Some(88720));
        assert_eq!(opp.liquidity_amount, Some(500_000));
    }

    #[test]
    #[should_panic(expected = "JIT fields only valid")]
    fn test_jit_fields_on_non_jit_panics_in_debug() {
        use alloy::primitives::address;
        let _opp = MevOpportunity::new(1, 0, Strategy::Sandwich, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
            .with_jit_fields(-100, 100, 500_000);
    }

    #[test]
    fn test_sandwich_fields_required() {
        use alloy::primitives::address;
        let opp = MevOpportunity::new(1, 0, Strategy::Sandwich, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
            .with_sandwich_fields(1, 2);
        assert_eq!(opp.victim_tx_index, Some(1));
        assert_eq!(opp.backrun_tx_index, Some(2));
    }

    #[test]
    fn test_mev_opportunity_sandwich_fields_roundtrip() {
        use alloy::primitives::address;
        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::Sandwich,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            gas_cost_wei: 0,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: Some(1),
            backrun_tx_index: Some(2),
        };
        let json = serde_json::to_string(&opp).unwrap();
        let deserialized: MevOpportunity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.victim_tx_index, Some(1));
        assert_eq!(deserialized.backrun_tx_index, Some(2));
        assert!(json.contains("\"victim_tx_index\""));
        assert!(json.contains("\"backrun_tx_index\""));

        // Verify fields are absent from serde output when None
        let no_sandwich = MevOpportunity {
            victim_tx_index: None,
            backrun_tx_index: None,
            ..opp
        };
        let json_no = serde_json::to_string(&no_sandwich).unwrap();
        assert!(!json_no.contains("victim_tx_index"));
    }
}
