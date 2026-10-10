//! ERC-20 Transfer and UniV2 Sync/Mint/Burn exclusion facts.
use alloy::primitives::U256;
use crate::utils::topic_address;

use crate::chain::events::{TRANSFER_TOPIC, V2_BURN_TOPIC, V2_MINT_TOPIC, V2_SYNC_TOPIC};
use crate::data::LogData;
use crate::explorer::types::{TransferFact, V2PairOpFact, V2PairOpKind};

/// Decode an ERC-20 Transfer fact from a receipt log.
pub fn decode_transfer(log: &LogData) -> Option<TransferFact> {
    if log.topics.len() < 3 || log.topics[0] != TRANSFER_TOPIC {
        return None;
    }
    if log.data.len() < 32 {
        return None;
    }
    Some(TransferFact {
        tx_index: 0,  // stamped by caller
        log_index: 0, // stamped by caller
        token: log.address,
        from: topic_address(log.topics[1]),
        to: topic_address(log.topics[2]),
        amount: U256::from_be_slice(&log.data[0..32]),
    })
}

/// Decode a UniV2-style Sync/Mint/Burn log (exclusion signal for skim).
///
/// Only the pool address and op kind are needed — amounts are irrelevant for
/// skim negatives.
pub fn decode_v2_pair_op(log: &LogData) -> Option<V2PairOpFact> {
    let topic0 = *log.topics.first()?;
    let kind = if topic0 == V2_SYNC_TOPIC {
        V2PairOpKind::Sync
    } else if topic0 == *V2_MINT_TOPIC {
        V2PairOpKind::Mint
    } else if topic0 == *V2_BURN_TOPIC {
        V2PairOpKind::Burn
    } else {
        return None;
    };
    Some(V2PairOpFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        kind,
    })
}

