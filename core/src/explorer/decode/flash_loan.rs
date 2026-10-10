//! Flash-loan event decoders.
use alloy::primitives::{Address, U256};

use crate::chain::events::{
    decode_balancer_flash, AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC,
    BALANCER_FLASH_LOAN_TOPIC, MORPHO_BLUE_FLASH_LOAN_TOPIC, UNI_V3_FLASH_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::FlashLoanFact;

/// Decode a flash-loan fact from a receipt log (Phase 2.2).
///
/// Covers Aave V2/V3, Balancer V2, Morpho Blue, and Uniswap V3 `Flash`.
/// Uni V4 has no discrete Flash event (unlock/callback flash accounting only),
/// so the Avalanche Uni flash path for plan P2.3 is V3.
pub fn decode_flash_loan(log: &LogData) -> Option<FlashLoanFact> {
    let topic0 = *log.topics.first()?;

    // Uniswap V3: Flash(address indexed sender, address indexed recipient,
    //   uint256 amount0, uint256 amount1, uint256 paid0, uint256 paid1).
    // Token identity needs the pool registry; store amount/fee on the
    // non-zero leg and leave `token = 0` for netting to fill via transfers.
    if topic0 == *UNI_V3_FLASH_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        let amount0 = U256::from_be_slice(&log.data[0..32]);
        let amount1 = U256::from_be_slice(&log.data[32..64]);
        let paid0 = U256::from_be_slice(&log.data[64..96]);
        let paid1 = U256::from_be_slice(&log.data[96..128]);
        let (amount, fee) = if !amount0.is_zero() || paid0 > paid1 {
            (amount0, paid0)
        } else {
            (amount1, paid1)
        };
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "uniswap_v3",
            initiator: Address::from_slice(&log.topics[1][12..]),
            recipient: Address::from_slice(&log.topics[2][12..]),
            token: Address::ZERO,
            amount,
            fee: Some(fee),
            provider: log.address,
        });
    }

    // Aave V3: FlashLoan(address indexed target, address initiator,
    //   address indexed asset, uint256 amount, uint8 mode, uint256 premium,
    //   uint16 referral) — topics [sig, target, asset], data 160 bytes.
    if topic0 == *AAVE_V3_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: Address::from_slice(&log.data[12..32]),
            recipient: Address::from_slice(&log.topics[1][12..]),
            token: Address::from_slice(&log.topics[2][12..]),
            amount: U256::from_be_slice(&log.data[32..64]),
            fee: Some(U256::from_be_slice(&log.data[96..128])),
            provider: log.address,
        });
    }

    // Aave V2: FlashLoan(address indexed target, address indexed initiator,
    //   address indexed asset, uint256 amount, uint256 premium, uint16 referral)
    //   — topics [sig, target, initiator, asset], data 96 bytes.
    if topic0 == *AAVE_V2_FLASH_LOAN_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v2",
            initiator: Address::from_slice(&log.topics[2][12..]),
            recipient: Address::from_slice(&log.topics[1][12..]),
            token: Address::from_slice(&log.topics[3][12..]),
            amount: U256::from_be_slice(&log.data[0..32]),
            fee: Some(U256::from_be_slice(&log.data[32..64])),
            provider: log.address,
        });
    }

    // Balancer V2: reuse the scanner decoder (topic + layout shared).
    if topic0 == *BALANCER_FLASH_LOAN_TOPIC {
        let rpc_log = logdata_to_rpc_log(log);
        let ev = decode_balancer_flash(&rpc_log)?;
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "balancer_v2",
            initiator: ev.initiator,
            token: ev.token,
            amount: ev.amount,
            fee: ev.fee,
            recipient: ev.target,
            provider: log.address,
        });
    }

    // Morpho Blue: FlashLoan(address indexed caller, address indexed token,
    //   uint256 assets) — 0% premium (§11 / P2.3 routing).
    if topic0 == *MORPHO_BLUE_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 32 {
            return None;
        }
        let caller = Address::from_slice(&log.topics[1][12..]);
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "morpho_blue",
            initiator: caller,
            recipient: caller,
            token: Address::from_slice(&log.topics[2][12..]),
            amount: U256::from_be_slice(&log.data[0..32]),
            fee: Some(U256::ZERO),
            provider: log.address,
        });
    }

    None
}

/// Minimal `LogData` → alloy `Log` bridge with placeholder block/tx metadata
/// (the explorer keys off its own tx/log indices; decoders only read the
/// event payload and topics).
fn logdata_to_rpc_log(log: &LogData) -> alloy::rpc::types::Log {
    let data = alloy::primitives::LogData::new_unchecked(log.topics.clone(), log.data.clone());
    alloy::rpc::types::Log {
        inner: alloy::primitives::Log {
            address: log.address,
            data,
        },
        block_number: Some(0),
        block_hash: Some(alloy::primitives::B256::ZERO),
        block_timestamp: None,
        transaction_hash: Some(alloy::primitives::B256::ZERO),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}

