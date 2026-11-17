//! First-class LB/DLMM action events: reward claims, bin-token batches and
//! bin reward accumulator updates.
//!
//! These decoders are intentionally strict. A `LogFamily` match that falls
//! through to `None` is *not* recorded as an unmapped log (see
//! `is_mapped_log`), so a permissive decoder would silently hide evidence.
//! Every accepted shape is checked down to padding and array lengths.
use alloy::primitives::{B256, U256};

use super::jit::decode_abi_u256_array;
use crate::chain::events::{
    LB_BIN_REWARD_UPDATED_TOPIC, LB_CLAIM_REWARDS_TOPIC, LB_TRANSFER_BATCH_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{
    LbRewardAccumulatorFact, LbTransferBatchFact, RewardClaimFact,
};
use crate::utils::topic_address;

fn is_padded_address(topic: &B256) -> bool {
    topic[..12].iter().all(|b| *b == 0)
}

/// `ClaimRewards(address indexed caller, address indexed reward, uint256 amount)`.
pub fn decode_reward_claim(log: &LogData) -> Option<RewardClaimFact> {
    if *log.topics.first()? != LB_CLAIM_REWARDS_TOPIC
        || log.topics.len() != 3
        || log.data.len() != 32
        || !is_padded_address(&log.topics[1])
        || !is_padded_address(&log.topics[2])
    {
        return None;
    }
    Some(RewardClaimFact {
        tx_index: 0,
        log_index: 0,
        emitter: log.address,
        claimer: topic_address(log.topics[1]),
        token: topic_address(log.topics[2]),
        amount: U256::from_be_slice(&log.data),
    })
}

/// `TransferBatch(address indexed operator, address indexed from,
/// address indexed to, uint256[] ids, uint256[] values)`.
pub fn decode_lb_transfer_batch(log: &LogData) -> Option<LbTransferBatchFact> {
    if *log.topics.first()? != LB_TRANSFER_BATCH_TOPIC
        || log.topics.len() != 4
        || !is_padded_address(&log.topics[1])
        || !is_padded_address(&log.topics[2])
        || !is_padded_address(&log.topics[3])
    {
        return None;
    }
    let ids = decode_abi_u256_array(&log.data, 0)?;
    let values = decode_abi_u256_array(&log.data, 1)?;
    if ids.is_empty() || ids.len() != values.len() {
        return None;
    }
    Some(LbTransferBatchFact {
        tx_index: 0,
        log_index: 0,
        emitter: log.address,
        operator: topic_address(log.topics[1]),
        from: topic_address(log.topics[2]),
        to: topic_address(log.topics[3]),
        ids,
        values,
    })
}

/// `BinRewardAccumulatorsUpdated(uint24 id, uint256[] rewards)` — no indexed
/// params: `id` is the first data word, the dynamic array offset the second.
pub fn decode_lb_reward_accumulator(log: &LogData) -> Option<LbRewardAccumulatorFact> {
    if *log.topics.first()? != LB_BIN_REWARD_UPDATED_TOPIC
        || log.topics.len() != 1
        || log.data.len() < 64
        || log.data[..29].iter().any(|b| *b != 0)
    {
        return None;
    }
    let rewards = decode_abi_u256_array(&log.data, 1)?;
    if rewards.is_empty() {
        return None;
    }
    let bin_id = u32::from_be_bytes([
        log.data[28],
        log.data[29],
        log.data[30],
        log.data[31],
    ]);
    Some(LbRewardAccumulatorFact {
        tx_index: 0,
        log_index: 0,
        emitter: log.address,
        bin_id,
        rewards,
    })
}
