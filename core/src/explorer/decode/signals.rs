//! Oracle, reserve, keeper, epoch, GMX, and ERC-4337 signal decoders.
use alloy::primitives::{Address, B256, U256};

use crate::chain::events::{
    AAVE_V3_RESERVE_DATA_UPDATED_TOPIC, BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC,
    CHAINLINK_ANSWER_UPDATED_TOPIC, CHAINLINK_LOG_TRIGGERED_TOPIC, CHAINLINK_UPKEEP_PERFORMED_TOPIC,
    GELATO_EXEC_SUCCESS_TOPIC, GMX_ADL_STATE_UPDATED_HASH, GMX_LIQUIDATE_POSITION_HASH,
    GMX_POSITION_IMPACT_POOL_DISTRIBUTED_HASH, NOTIFY_REWARD_TOPIC, USER_OPERATION_EVENT_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{
    EpochRewardFact, GmxEventFact, KeeperFact, OracleUpdateFact, RateCacheFact, ReserveDataFact,
    UserOpFact,
};

/// Decode Chainlink AggregatorV3 `AnswerUpdated` (plan P1.4 / §17.8.4 mode A/B).
///
/// Mode A uses the feed emitter for co-block poke; mode B compares `answer`
/// against the prior stored answer for pre-poke divergence.
pub fn decode_oracle_update(log: &LogData) -> Option<OracleUpdateFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *CHAINLINK_ANSWER_UPDATED_TOPIC || log.topics.len() < 3 {
        return None;
    }
    Some(OracleUpdateFact {
        tx_index: 0,
        log_index: 0,
        feed: log.address,
        answer: signed_answer_from_topic(&log.topics[1]),
    })
}

/// Interpret a 32-byte indexed int256 topic as i128 (Chainlink answers fit).
fn signed_answer_from_topic(topic: &B256) -> i128 {
    let bytes = topic.as_slice();
    let mut lo = [0u8; 16];
    lo.copy_from_slice(&bytes[16..32]);
    // ABI-indexed int256 sign-extends into the high 16 bytes (0x00.. or 0xff..).
    i128::from_be_bytes(lo)
}

/// Decode Balancer ComposableStablePool `TokenRateCacheUpdated` (plan P3.2).
///
/// Mode A: a same-block rate refresh clears the staleness label on Balancer
/// arbs; absence of this event while a Balancer leg closes a cross-venue cycle
/// is the inferred staleness cause.
pub fn decode_rate_cache_update(log: &LogData) -> Option<RateCacheFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC || log.topics.len() < 2 {
        return None;
    }
    if log.data.len() < 32 {
        return None;
    }
    Some(RateCacheFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        token_index: u64::try_from(U256::from_be_slice(log.topics[1].as_slice()))
            .unwrap_or(u64::MAX),
        rate: U256::from_be_slice(&log.data[0..32]),
    })
}

/// Decode Aave V3 `ReserveDataUpdated` (plan P1.1 / §17.8.4).
///
/// Mode A in-block signal that the debt reserve's borrow index moved; paired
/// with a price-flat check in [`crate::explorer::interest_attr`].
pub fn decode_reserve_data(log: &LogData) -> Option<ReserveDataFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *AAVE_V3_RESERVE_DATA_UPDATED_TOPIC {
        return None;
    }
    if log.topics.len() < 2 || log.data.len() < 160 {
        return None;
    }
    Some(ReserveDataFact {
        tx_index: 0,
        log_index: 0,
        reserve: Address::from_slice(&log.topics[1][12..]),
        // data: liquidityRate, stableBorrowRate, variableBorrowRate, …
        variable_borrow_rate: U256::from_be_slice(&log.data[64..96]),
    })
}

/// Decode Gelato Automate / Chainlink Automation keeper executions
/// (plan P1.5 / §17.8.9 mode A).
///
/// P&L basis `F` comes from the explicit fee field when present
/// (`ExecSuccess.txFee`, `UpkeepPerformed.totalPayment`); `LogTriggered`
/// carries no fee → fee stays `None` and residual `R` is left to transfers.
pub fn decode_keeper(log: &LogData) -> Option<KeeperFact> {
    let topic0 = *log.topics.first()?;

    if topic0 == *GELATO_EXEC_SUCCESS_TOPIC {
        // data: txFee, feeToken, execAddress, offset(execData), taskId, callSuccess
        if log.data.len() < 192 {
            return None;
        }
        let fee = U256::from_be_slice(&log.data[0..32]);
        let fee_token = Address::from_slice(&log.data[44..64]);
        return Some(KeeperFact {
            tx_index: 0,
            log_index: 0,
            protocol: "gelato",
            emitter: log.address,
            fee: Some(fee),
            fee_token: Some(fee_token),
        });
    }

    if topic0 == *CHAINLINK_UPKEEP_PERFORMED_TOPIC {
        // topics: [sig, id, success]; data starts with totalPayment (uint96 word)
        if log.topics.len() < 3 || log.data.len() < 32 {
            return None;
        }
        return Some(KeeperFact {
            tx_index: 0,
            log_index: 0,
            protocol: "chainlink_automation",
            emitter: log.address,
            fee: Some(U256::from_be_slice(&log.data[0..32])),
            fee_token: None, // LINK / native settled off the event args
        });
    }

    if topic0 == *CHAINLINK_LOG_TRIGGERED_TOPIC {
        if log.topics.len() < 4 {
            return None;
        }
        return Some(KeeperFact {
            tx_index: 0,
            log_index: 0,
            protocol: "chainlink_automation",
            emitter: log.address,
            fee: None,
            fee_token: None,
        });
    }

    None
}

/// Decode Solidly/Pharaoh/Blackhole `NotifyReward` (plan P3.15 / §7.5).
///
/// Mode A epoch fingerprint: gauge emission/bribe notification co-occurring
/// with realized arbs on venue pools.
pub fn decode_epoch_reward(log: &LogData) -> Option<EpochRewardFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *NOTIFY_REWARD_TOPIC || log.topics.len() < 3 || log.data.len() < 32 {
        return None;
    }
    Some(EpochRewardFact {
        tx_index: 0,
        log_index: 0,
        emitter: log.address,
        reward_token: Address::from_slice(&log.topics[2][12..]),
        amount: U256::from_be_slice(&log.data[0..32]),
    })
}

/// Decode GMX V2 EventEmitter ADL/liquidation-adjacent logs (plan P3.7).
///
/// `eventNameHash` is topics[1] (Solidity `string indexed`). Optional
/// emitter allowlist is applied at classify/ingest via config.
pub fn decode_gmx_event(log: &LogData) -> Option<GmxEventFact> {
    if log.topics.len() < 2 {
        return None;
    }
    let name = log.topics[1];
    let kind = if name == *GMX_ADL_STATE_UPDATED_HASH {
        "adl"
    } else if name == *GMX_LIQUIDATE_POSITION_HASH {
        "liquidation"
    } else if name == *GMX_POSITION_IMPACT_POOL_DISTRIBUTED_HASH {
        "impact"
    } else {
        return None;
    };
    Some(GmxEventFact {
        tx_index: 0,
        log_index: 0,
        emitter: log.address,
        kind,
    })
}

/// Decode ERC-4337 EntryPoint `UserOperationEvent` (plan P3.11 / §7.4).
pub fn decode_user_op(log: &LogData) -> Option<UserOpFact> {
    let topic0 = *log.topics.first()?;
    if topic0 != *USER_OPERATION_EVENT_TOPIC || log.topics.len() < 4 || log.data.len() < 128 {
        return None;
    }
    // data: nonce, success, actualGasCost, actualGasUsed
    let success_word = U256::from_be_slice(&log.data[32..64]);
    Some(UserOpFact {
        tx_index: 0,
        log_index: 0,
        entry_point: log.address,
        sender: Address::from_slice(&log.topics[2][12..]),
        paymaster: Address::from_slice(&log.topics[3][12..]),
        actual_gas_cost: U256::from_be_slice(&log.data[64..96]),
        success: !success_word.is_zero(),
    })
}

