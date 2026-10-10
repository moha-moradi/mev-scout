//! Lending-protocol liquidation and BuyCollateral decoders.
use alloy::primitives::{Address, U256};
use crate::utils::{abi_word_address, topic_address};

use crate::chain::events::{
    AAVE_V3_LIQUIDATION_CALL_TOPIC, COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, COMPOUND_V3_ABSORB_TOPIC,
    COMPOUND_V3_BUY_COLLATERAL_TOPIC, EULER_V2_LIQUIDATE_TOPIC, MORPHO_BLUE_LIQUIDATE_TOPIC,
    SILO_V2_LIQUIDATION_CALL_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{BuyCollateralFact, LiquidationFact};

/// Decode a liquidation fact via the per-protocol event registry.
///
/// Mode A fingerprints ( /): Aave-family `LiquidationCall` (Spark
/// remapped by emitter address via [`remap_liquidation_protocol`]), Compound V3
/// `Absorb`, Compound V2 `LiquidateBorrow` (Benqi remapped by emitter address),
/// Morpho Blue `Liquidate`, Silo V2 `LiquidationCall`, Euler V2 `Liquidate`.
pub fn decode_liquidation(log: &LogData) -> Option<LiquidationFact> {
    let topic0 = *log.topics.first()?;
    if topic0 == *AAVE_V3_LIQUIDATION_CALL_TOPIC {
        // topics: [sig, collateralAsset, debtAsset, user]
        // data: debtToCover, liquidatedCollateralAmount, receiveAToken
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            // Spark and other Aave-V3 ABI aliases remap via emitter address.
            protocol: "aave_v3",
            emitter: log.address,
            user: topic_address(log.topics[3]),
            // `LiquidationCall` does not carry the liquidator (msg.sender);
            // attribution falls back to the tx sender at classify time.
            liquidator: Address::ZERO,
            collateral_asset: topic_address(log.topics[1]),
            debt_asset: topic_address(log.topics[2]),
            collateral_amount: U256::from_be_slice(&log.data[32..64]),
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
            bad_debt_assets: U256::ZERO,
        });
    }
    if topic0 == *COMPOUND_V3_ABSORB_TOPIC {
        // topics: [sig, absorber]; data: (borrower[], basePaid[], basePaidTotal)
        if log.topics.len() < 2 || log.data.len() < 84 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v3",
            emitter: log.address,
            user: abi_word_address(&log.data, 0),
            liquidator: topic_address(log.topics[1]),
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_amount: U256::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[52..84]),
            bad_debt_assets: U256::ZERO,
        });
    }
    if topic0 == *COMPOUND_V2_LIQUIDATE_BORROW_TOPIC {
        // topics: [sig, liquidator, borrower]
        // data: repayAmount, cTokenCollateral (word), seizeTokens
        if log.topics.len() < 3 || log.data.len() < 96 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v2",
            emitter: log.address,
            user: topic_address(log.topics[2]),
            liquidator: topic_address(log.topics[1]),
            // Repaid market is the emitting cToken; seized collateral is a
            // different cToken address encoded in the data.
            debt_asset: log.address,
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
            collateral_asset: abi_word_address(&log.data, 1),
            collateral_amount: U256::from_be_slice(&log.data[64..96]),
            bad_debt_assets: U256::ZERO,
        });
    }
    if topic0 == *MORPHO_BLUE_LIQUIDATE_TOPIC {
        // topics: [sig, id, caller, borrower]
        // data: repaidAssets, repaidShares, seizedAssets, badDebtAssets, badDebtShares
        if log.topics.len() < 4 || log.data.len() < 96 {
            return None;
        }
        let bad_debt = if log.data.len() >= 128 {
            U256::from_be_slice(&log.data[96..128])
        } else {
            U256::ZERO
        };
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "morpho_blue",
            emitter: log.address,
            user: topic_address(log.topics[3]),
            liquidator: topic_address(log.topics[2]),
            // Market loan/collateral tokens live off-event (market id); transfer
            // reconciliation fills them when present.
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
            collateral_amount: U256::from_be_slice(&log.data[64..96]),
            bad_debt_assets: bad_debt,
        });
    }
    if topic0 == *SILO_V2_LIQUIDATION_CALL_TOPIC {
        // topics: [sig, liquidator, silo, borrower]
        // data: repayDebtAssets, withdrawCollateral, receiveSToken
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "silo_v2",
            emitter: log.address,
            user: topic_address(log.topics[3]),
            liquidator: topic_address(log.topics[1]),
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
            collateral_amount: U256::from_be_slice(&log.data[32..64]),
            bad_debt_assets: U256::ZERO,
        });
    }
    if topic0 == *EULER_V2_LIQUIDATE_TOPIC {
        // topics: [sig, liquidator, violator]
        // data: collateral, repayAssets, yieldBalance
        if log.topics.len() < 3 || log.data.len() < 96 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "euler_v2",
            emitter: log.address,
            user: topic_address(log.topics[2]),
            liquidator: topic_address(log.topics[1]),
            collateral_asset: abi_word_address(&log.data, 0),
            // Debt asset is the emitting vault's underlying — unknown from the
            // event alone; leave zero for transfer reconciliation.
            debt_asset: Address::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[32..64]),
            collateral_amount: U256::from_be_slice(&log.data[64..96]),
            bad_debt_assets: U256::ZERO,
        });
    }
    None
}

/// Remap a decoded liquidation's protocol label by emitting-contract address.
///
/// Mode A ( / plan P0.2): protocols whose event topic0 is shared with
/// another deployment are distinguished only by the emitter — Spark reuses
/// the Aave V3 `LiquidationCall` topic, Benqi qiTokens reuse the Compound V2
/// `LiquidateBorrow` topic. `aliases` maps emitter → protocol label; the
/// lookup is unconditional because emitter addresses of distinct protocols
/// never collide.
pub fn remap_liquidation_protocol(
    liq: &mut LiquidationFact,
    aliases: &std::collections::HashMap<Address, &'static str>,
) {
    if let Some(label) = aliases.get(&liq.emitter) {
        liq.protocol = label;
    }
}

/// Decode Compound V3 `BuyCollateral` ( / plan P0.3).
///
/// Mode A: `BuyCollateral(address buyer, address asset, uint256 baseAmount,
/// uint256 collateralAmount)` with buyer/asset indexed.
pub fn decode_buy_collateral(log: &LogData) -> Option<BuyCollateralFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *COMPOUND_V3_BUY_COLLATERAL_TOPIC {
        return None;
    }
    if log.topics.len() < 3 || log.data.len() < 64 {
        return None;
    }
    Some(BuyCollateralFact {
        tx_index: 0,
        log_index: 0,
        protocol: "compound_v3",
        emitter: log.address,
        buyer: topic_address(log.topics[1]),
        collateral_asset: topic_address(log.topics[2]),
        base_amount: U256::from_be_slice(&log.data[0..32]),
        collateral_amount: U256::from_be_slice(&log.data[32..64]),
    })
}

