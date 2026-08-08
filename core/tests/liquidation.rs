// LiquidationDetector tests — synthetic, no RPC required.
//
// Covers both detection modes:
//   1. Reactive — an on-chain LiquidationCall event yields an opportunity.
//   2. Proactive — Supply/Borrow events build an underwater user position
//      (health factor < 1) that yields an opportunity even with no event.
// Plus unit checks on compute_health_factor.

use alloy::primitives::{address, keccak256, Address, B256, Bytes, U256};
use mev_scout_core::data::ExecutedLog;
use mev_scout_core::mev::detectors::liquidation::{LiquidationDetector, compute_health_factor};
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::types::{GasConfig, Strategy};

mod common;
use common::{make_pool, usdc, wmatic};

const LIQ_SIG: &str = "LiquidationCall(address,address,address,uint256,uint256,address,bool)";
const SUPPLY_SIG: &str = "Supply(address,address,address,uint256,uint16)";
const BORROW_SIG: &str = "Borrow(address,address,address,uint256,uint8,uint256,uint16)";

fn topic_from_address(addr: Address) -> B256 {
    let mut t = [0u8; 32];
    t[12..32].copy_from_slice(addr.as_slice());
    B256::from(t)
}

/// Build an ExecutedLog with `words` as 32-byte big-endian data words.
fn make_log(address: Address, topics: Vec<B256>, words: &[u128]) -> ExecutedLog {
    let mut data = Vec::with_capacity(words.len() * 32);
    for w in words {
        let mut word = [0u8; 32];
        word[16..32].copy_from_slice(&w.to_be_bytes());
        data.extend_from_slice(&word);
    }
    ExecutedLog {
        address,
        topics,
        data: Bytes::from(data),
    }
}

fn liq_call_log(collateral: Address, debt: Address, user: Address, debt_to_cover: u128, seized: u128) -> ExecutedLog {
    make_log(
        address!("794a61358d6845594f94dc1db02a252b5b4814ad"), // Aave V3 Pool (Polygon)
        vec![
            keccak256(LIQ_SIG),
            topic_from_address(collateral),
            topic_from_address(debt),
            topic_from_address(user),
        ],
        &[debt_to_cover, seized, 0, 0], // debtToCover, liquidatedCollateral, liquidator, receiveAToken
    )
}

fn supply_log(reserve: Address, user: Address, on_behalf: Address, amount: u128) -> ExecutedLog {
    make_log(
        address!("794a61358d6845594f94dc1db02a252b5b4814ad"),
        vec![
            keccak256(SUPPLY_SIG),
            topic_from_address(reserve),
            topic_from_address(user),
            topic_from_address(on_behalf),
        ],
        &[amount, 0], // amount, referralCode
    )
}

fn borrow_log(reserve: Address, user: Address, on_behalf: Address, amount: u128) -> ExecutedLog {
    make_log(
        address!("794a61358d6845594f94dc1db02a252b5b4814ad"),
        vec![
            keccak256(BORROW_SIG),
            topic_from_address(reserve),
            topic_from_address(user),
            topic_from_address(on_behalf),
        ],
        &[amount, 0, 0, 0], // amount, interestRateMode, borrowRate, referralCode
    )
}

/// PoolManager with a USDC/WMATIC V2 pool so native normalization works:
/// reserve0 = 1e12 USDC, reserve1 = 2e18 WMATIC → 1e9 USDC ≈ 2e15 WMATIC wei.
fn native_pools() -> PoolManager {
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        address!("cccccccccccccccccccccccccccccccccccccccc"),
        usdc(),
        wmatic(),
        1_000_000_000_000, // 1e12 USDC
        2_000_000_000_000_000_000, // 2e18 WMATIC
    ));
    pm.with_wrapped_native(wmatic())
}

fn default_gas() -> GasConfig {
    GasConfig::default()
}

/// Test 1: compute_health_factor unit checks.
#[test]
fn test_health_factor_formula() {
    // Zero debt → healthy (MAX)
    assert_eq!(compute_health_factor(1_000, 0, 8000), f64::MAX);

    // HF = (collateral * threshold) / debt
    let hf = compute_health_factor(100, 50, 8000);
    assert!((hf - 1.6).abs() < 1e-9, "expected 1.6, got {hf}");

    // Underwater: collateral * threshold < debt
    let hf = compute_health_factor(100, 90, 8000);
    assert!(hf < 1.0, "expected underwater, got {hf}");

    // Threshold of 100% → HF = collateral / debt
    let hf = compute_health_factor(90, 100, 10000);
    assert!((hf - 0.9).abs() < 1e-9, "expected 0.9, got {hf}");
}

/// Test 2: Reactive mode — a real LiquidationCall event is emitted as an opportunity.
#[test]
fn test_liquidation_reactive_event() {
    let user = address!("1111111111111111111111111111111111111111");
    let pm = native_pools();

    // Liquidator seizes 2e15 WMATIC (collateral) covering 1e9 USDC debt.
    // collateral_native ≈ 2e15, debt_native ≈ 2e15 * (1e9/1e12)... ~1.99e15 → profit > 0.
    let log = liq_call_log(wmatic(), usdc(), user, 1_000_000_000, 2_000_000_000_000_000);

    let mut detector = LiquidationDetector::new(42);
    detector.process_tx(0, &[log]);

    let opps = detector.detect(&pm, 1_700_000_000, 30_000_000_000, default_gas());
    assert_eq!(opps.len(), 1, "LiquidationCall should yield exactly 1 opportunity");

    let opp = &opps[0];
    assert_eq!(opp.strategy, Strategy::Liquidation);
    assert_eq!(opp.block_number, 42);
    assert_eq!(opp.token_in, usdc(), "token_in should be the debt asset");
    assert_eq!(opp.token_out, wmatic(), "token_out should be the collateral asset");
    assert_eq!(opp.input_amount, U256::from(1_000_000_000u128));
    assert!(opp.expected_profit > U256::ZERO, "expected positive profit");
    assert!(opp.gas_cost_wei > 0, "expected positive gas cost");
    assert_eq!(opp.timestamp, 1_700_000_000);
}

/// Test 3: Proactive mode — an underwater Supply/Borrow position yields an opportunity.
#[test]
fn test_liquidation_proactive_underwater() {
    let user = address!("2222222222222222222222222222222222222222");
    let pm = native_pools();

    // Collateral: 1e15 WMATIC → native 1e15.
    // Debt:       1e9 USDC     → native ≈ 1.99e15.
    // HF ≈ 0.8 * 1e15 / 1.99e15 ≈ 0.40 < 1 → liquidatable.
    let mut detector = LiquidationDetector::new(42);
    detector.process_tx(0, &[supply_log(wmatic(), user, user, 1_000_000_000_000_000)]);
    detector.process_tx(1, &[borrow_log(usdc(), user, user, 1_000_000_000)]);

    let opps = detector.detect(&pm, 1_700_000_000, 30_000_000_000, default_gas());
    assert_eq!(opps.len(), 1, "Underwater position should yield exactly 1 opportunity");

    let opp = &opps[0];
    assert_eq!(opp.strategy, Strategy::Liquidation);
    assert_eq!(opp.token_in, usdc(), "token_in should be the debt asset");
    assert_eq!(opp.token_out, wmatic(), "token_out should be the collateral asset");
    assert!(opp.expected_profit > U256::ZERO, "expected positive profit");
    // Close factor is 50% of debt (1e9 USDC → 5e8 USDC)
    assert_eq!(opp.input_amount, U256::from(500_000_000u128));
}

/// Test 4: Proactive mode — a healthy position produces no opportunity.
#[test]
fn test_liquidation_proactive_healthy() {
    let user = address!("3333333333333333333333333333333333333333");
    let pm = native_pools();

    // Collateral: 1e16 WMATIC → native 1e16 (10x the debt value).
    // HF ≈ 0.8 * 1e16 / 1.99e15 ≈ 4.0 ≥ 1 → healthy, no opportunity.
    let mut detector = LiquidationDetector::new(42);
    detector.process_tx(0, &[supply_log(wmatic(), user, user, 10_000_000_000_000_000)]);
    detector.process_tx(1, &[borrow_log(usdc(), user, user, 1_000_000_000)]);

    let opps = detector.detect(&pm, 1_700_000_000, 30_000_000_000, default_gas());
    assert!(opps.is_empty(), "Healthy position should produce no opportunities; got {}", opps.len());
}
