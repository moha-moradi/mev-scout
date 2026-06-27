//! Wire-format data types mapping raw Ethereum JSON-RPC responses to Rust structs.
//!
//! These structs map directly to raw Ethereum JSON-RPC response fields and are
//! intentionally kept close to the underlying RPC schema so conversions remain
//! obvious. Internal extended types (`ExecutedTx`, `ExecutedLog`) carry fields
//! populated by the block replayer that do not appear on the wire.

use alloy::primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Raw block header fields from `eth_getBlockByNumber`.
pub struct BlockData {
    pub number: u64,
    pub hash: B256,
    pub timestamp: u64,
    pub base_fee_per_gas: Option<u128>,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub coinbase: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Raw transaction fields from `eth_getTransactionByHash`.
pub struct TxData {
    pub hash: B256,
    pub index: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub input: Bytes,
    pub value: U256,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: Option<u128>,
    pub nonce: u64,
    pub access_list: Vec<AccessListItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// An entry in a transaction's access list (address + storage slots).
pub struct AccessListItem {
    pub address: Address,
    pub slots: Vec<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Receipt fields from `eth_getTransactionReceipt`.
pub struct ReceiptData {
    pub tx_hash: B256,
    pub tx_index: u64,
    pub status: bool,
    pub gas_used: u64,
    pub cumulative_gas_used: u64,
    pub logs: Vec<LogData>,
    pub contract_address: Option<Address>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Single log entry emitted during transaction execution.
pub struct LogData {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountData {
    pub nonce: u64,
    pub balance: U256,
    pub code_hash: B256,
}

#[derive(Debug, Clone)]
/// Transaction as executed during block replay (includes emulated state).
pub struct ExecutedTx {
    pub tx_hash: B256,
    pub index: u64,
    pub status: bool,
    pub gas_used: u64,
    pub gas_effective: u128,
    pub logs: Vec<ExecutedLog>,
    pub output: Bytes,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
/// Single log entry produced during EVM replay (address + topics + data).
pub struct ExecutedLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, bytes, b256, U256};

    fn sample_address() -> Address {
        address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
    }

    fn sample_b256() -> B256 {
        b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")
    }

    fn sample_bytes() -> Bytes {
        bytes!("deadbeef")
    }

    #[test]
    fn test_block_data_serde_roundtrip() {
        let data = BlockData {
            number: 42,
            hash: sample_b256(),
            timestamp: 1234567890,
            base_fee_per_gas: Some(50_000_000_000u128),
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            coinbase: sample_address(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: BlockData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.number, 42);
        assert_eq!(restored.hash, data.hash);
        assert_eq!(restored.timestamp, 1234567890);
        assert_eq!(restored.base_fee_per_gas, Some(50_000_000_000u128));
        assert_eq!(restored.gas_limit, 30_000_000);
        assert_eq!(restored.gas_used, 15_000_000);
        assert_eq!(restored.coinbase, sample_address());
    }

    #[test]
    fn test_block_data_null_base_fee() {
        let data = BlockData {
            number: 1,
            hash: B256::ZERO,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_limit: 0,
            gas_used: 0,
            coinbase: Address::ZERO,
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: BlockData = serde_json::from_str(&json).unwrap();
        assert!(restored.base_fee_per_gas.is_none());
    }

    #[test]
    fn test_tx_data_serde_roundtrip() {
        let data = TxData {
            hash: sample_b256(),
            index: 5,
            from: sample_address(),
            to: Some(sample_address()),
            input: sample_bytes(),
            value: U256::from(1_000_000_000_000_000_000u128),
            gas_limit: 100_000,
            max_fee_per_gas: 50_000_000_000,
            max_priority_fee_per_gas: Some(1_000_000_000),
            nonce: 17,
            access_list: vec![AccessListItem {
                address: sample_address(),
                slots: vec![sample_b256()],
            }],
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: TxData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.hash, data.hash);
        assert_eq!(restored.index, 5);
        assert_eq!(restored.from, sample_address());
        assert_eq!(restored.to, Some(sample_address()));
        assert_eq!(restored.input, sample_bytes());
        assert_eq!(restored.value, U256::from(1_000_000_000_000_000_000u128));
        assert_eq!(restored.gas_limit, 100_000);
        assert_eq!(restored.nonce, 17);
        assert_eq!(restored.access_list.len(), 1);
        assert_eq!(restored.access_list[0].address, sample_address());
    }

    #[test]
    fn test_tx_data_no_to_no_priority_fee() {
        let data = TxData {
            hash: B256::ZERO,
            index: 0,
            from: Address::ZERO,
            to: None,
            input: Bytes::default(),
            value: U256::ZERO,
            gas_limit: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: None,
            nonce: 0,
            access_list: vec![],
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: TxData = serde_json::from_str(&json).unwrap();
        assert!(restored.to.is_none());
        assert!(restored.max_priority_fee_per_gas.is_none());
        assert!(restored.access_list.is_empty());
    }

    #[test]
    fn test_receipt_data_serde_roundtrip() {
        let data = ReceiptData {
            tx_hash: sample_b256(),
            tx_index: 3,
            status: true,
            gas_used: 42_000,
            cumulative_gas_used: 100_000,
            logs: vec![LogData {
                address: sample_address(),
                topics: vec![sample_b256(), B256::ZERO],
                data: sample_bytes(),
            }],
            contract_address: Some(sample_address()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: ReceiptData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tx_hash, data.tx_hash);
        assert_eq!(restored.tx_index, 3);
        assert!(restored.status);
        assert_eq!(restored.gas_used, 42_000);
        assert_eq!(restored.logs.len(), 1);
        assert_eq!(restored.logs[0].topics.len(), 2);
        assert_eq!(restored.contract_address, Some(sample_address()));
    }

    #[test]
    fn test_receipt_data_failed_tx() {
        let data = ReceiptData {
            tx_hash: B256::ZERO,
            tx_index: 0,
            status: false,
            gas_used: 0,
            cumulative_gas_used: 0,
            logs: vec![],
            contract_address: None,
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: ReceiptData = serde_json::from_str(&json).unwrap();
        assert!(!restored.status);
        assert!(restored.logs.is_empty());
        assert!(restored.contract_address.is_none());
    }

    #[test]
    fn test_log_data_serde_roundtrip() {
        let data = LogData {
            address: sample_address(),
            topics: vec![sample_b256()],
            data: sample_bytes(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: LogData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.address, sample_address());
        assert_eq!(restored.topics, vec![sample_b256()]);
        assert_eq!(restored.data, sample_bytes());
    }

    #[test]
    fn test_account_data_serde_roundtrip() {
        let data = AccountData {
            nonce: 7,
            balance: U256::from(1_000_000_000_000_000_000u128),
            code_hash: sample_b256(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: AccountData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.nonce, 7);
        assert_eq!(restored.balance, U256::from(1_000_000_000_000_000_000u128));
        assert_eq!(restored.code_hash, sample_b256());
    }

    #[test]
    fn test_log_data_multiple_topics() {
        let data = LogData {
            address: Address::ZERO,
            topics: vec![B256::ZERO, sample_b256(), B256::ZERO],
            data: Bytes::default(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let restored: LogData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.topics.len(), 3);
    }
}
