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
/// Normalized log entry stored in the `logs` table.
pub struct NormalizedLog {
    pub block_number: u64,
    pub tx_index: u64,
    pub log_index: u64,
    pub address: Address,
    pub topic0: Option<B256>,
    pub topic1: Option<B256>,
    pub topic2: Option<B256>,
    pub topic3: Option<B256>,
    pub data: Bytes,
    pub erc20_amount: Option<U256>,
    pub event_sig: Option<String>,
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

