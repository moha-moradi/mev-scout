//! EVM block replay via revm — the execution engine for historical backtests.
//!
//! This module replays cached block data through revm to reconstruct EVM state
//! transaction-by-transaction. It is the performance-critical path of the
//! backtest engine.
//!
//! ## Key components
//! - [`BlockReplayer`] — high-level replay interface used by `BacktestRunner`
//! - [`CachedRpcDb`] — lazy-fetch database bridging SQLite cache and RPC for
//!   revm's `Database` trait
//! - [`StateSnapshot`] — forkable state wrapper for snapshot/rollback patterns
//!
//! ## Polygon special handling
//! Chain 137 (Polygon) requires BLS12-377 precompile registration and state
//! receiver stubs. See [`register_polygon_precompiles`] and
//! [`spec_id_for_block`] for details.
//!
//! ## Receipt verification
//! After each transaction, `verify_receipt()` compares the revm execution
//! result against cached receipts (status, gas used, logs). Polygon system
//! logs from `0x1001` and `0x1010` are filtered during comparison.

use alloy::primitives::{address, keccak256, Address, B256, Bytes, U256};
use revm::bytecode::Bytecode;
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::result::{ExecutionResult, ResultGas};
use revm::context_interface::transaction::{AccessList, AccessListItem};
use revm::database::CacheDB;
use revm::handler::{ExecuteCommitEvm, MainBuilder, MainContext};
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::primitives::hardfork::SpecId;
use revm::state::AccountInfo;
use revm::Context;

use crate::cache::SqliteStore;
use crate::data::{BlockData, ExecutedLog, ExecutedTx, LogData, ReceiptData, TxData};
use crate::rpc::RpcClient;

pub use super::db::*;

/// Polygon BLS12-377 precompile addresses (Heimdall fork)
const BLS12_377_ADDRESSES: [u8; 4] = [0x09, 0x0a, 0x0b, 0x0c];
/// Polygon state receiver system contract
const STATE_RECEIVER: Address = address!("0000000000000000000000000000000000001001");

fn addr_from_last_byte(b: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = b;
    Address::from(bytes)
}

/// Select EVM spec ID based on chain and block number.
pub fn spec_id_for_block(chain_id: u64, block_number: u64) -> SpecId {
    match chain_id {
        137 => {
            if block_number >= 50_523_000 {
                SpecId::CANCUN
            } else if block_number >= 23_850_000 {
                SpecId::LONDON
            } else {
                SpecId::BERLIN
            }
        }
        _ => SpecId::NEXT,
    }
}



/// Register Polygon-specific precompiles and system contracts.
pub fn register_polygon_precompiles(
    db: &mut CacheDB<CachedRpcDb>,
    block_num: u64,
) -> anyhow::Result<()> {
    let prev_block = block_num.saturating_sub(1);
        let (rpc, handle) = {
            let inner = &db.db;
            (inner.rpc().clone(), inner.handle().clone())
        };

    let f09 = rpc.get_code_no_retry(addr_from_last_byte(0x09), prev_block);
    let f0a = rpc.get_code_no_retry(addr_from_last_byte(0x0a), prev_block);
    let f0b = rpc.get_code_no_retry(addr_from_last_byte(0x0b), prev_block);
    let f0c = rpc.get_code_no_retry(addr_from_last_byte(0x0c), prev_block);
    let (code_09, code_0a, code_0b, code_0c) = tokio::task::block_in_place(|| {
        handle.block_on(async { futures::join!(f09, f0a, f0b, f0c) })
    });
    let code_09 = code_09.unwrap_or_default();
    let code_0a = code_0a.unwrap_or_default();
    let code_0b = code_0b.unwrap_or_default();
    let code_0c = code_0c.unwrap_or_default();

    let codes = [
        (0x09, code_09),
        (0x0a, code_0a),
        (0x0b, code_0b),
        (0x0c, code_0c),
    ];

    for (b, code) in &codes {
        let addr = addr_from_last_byte(*b);
        if !code.is_empty() {
            tracing::info!(
                "Registering BLS12-377 precompile at {} with {} bytes of code",
                addr,
                code.len()
            );
        }
        let bytecode = Bytecode::new_raw(code.clone());
        let hash = if code.is_empty() {
            KECCAK_EMPTY
        } else {
            keccak256(code)
        };
        db.insert_account_info(
            addr,
            AccountInfo {
                nonce: 0,
                balance: U256::ZERO,
                code: Some(bytecode),
                code_hash: hash,
                account_id: None,
            },
        );
    }

    // State receiver (0x1001) is a Bor precompile, not an EVM contract.
    // Always register as no-op stub regardless of eth_getCode result.
    let bytecode = Bytecode::new_raw(Bytes::new());
    db.insert_account_info(
        STATE_RECEIVER,
        AccountInfo {
            nonce: 0,
            balance: U256::ZERO,
            code: Some(bytecode),
            code_hash: KECCAK_EMPTY,
            account_id: None,
        },
    );

    for b in 0x01u8..=0x1f {
        if BLS12_377_ADDRESSES.contains(&b) {
            continue;
        }
        let addr = addr_from_last_byte(b);
        if let Ok(code) = tokio::task::block_in_place(|| handle.block_on(rpc.get_code_no_retry(addr, prev_block))) {
            if !code.is_empty() {
                tracing::warn!(
                    "Unrecognised non-empty contract at precompile-range address {} ({} bytes)",
                    addr,
                    code.len()
                );
            }
        }
    }

    Ok(())
}

/// Block replayer that replays historical blocks through revm for MEV detection.
///
/// This is the primary interface between `BacktestRunner` and the EVM. It
/// loads cached block data (header, transactions, receipts) from SQLite and
/// replays them through revm with `CachedRpcDb` for lazy state fetching.
///
/// ## Replay modes
/// - `replay_to()` — replay up to a specific tx index (used by CLI `replay`)
/// - `replay_block()` — replay entire block
/// - `replay_each()` — replay with an `on_tx` callback after each tx
/// - `replay_each_filtered()` — replay with a filter to skip non-pool txs
///
/// The filtered mode is the critical performance optimization: most
/// transactions in a block do not interact with tracked DEX pools, and
/// skipping EVM execution for them reduces backtest time dramatically.
///
/// ## Polygon handling
/// For chain 137, BLS12-377 precompiles and the state receiver (0x1001)
/// are registered before each block replay. The EVM spec ID (Berlin,
/// London, Cancun) is selected per block number.
pub struct BlockReplayer {
    handle: tokio::runtime::Handle,
    cache: SqliteStore,
    rpc: RpcClient,
    chain_id: u64,
}

impl BlockReplayer {
    pub fn new(
        handle: tokio::runtime::Handle,
        cache: SqliteStore,
        rpc: RpcClient,
        chain_id: u64,
    ) -> Self {
        BlockReplayer {
            handle,
            cache,
            rpc,
            chain_id,
        }
    }

    /// Load just the txs for a block (used by CLI to count tx count before replay).
    pub fn load_txs(&self, block_num: u64) -> anyhow::Result<Vec<TxData>> {
        self.cache
            .get_txs(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Txs for block {} not found in cache", block_num))
    }

    pub fn load_block_data(&self, block_num: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block = self
            .cache
            .get_block(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Block {} not found in cache", block_num))?;
        let txs = self
            .cache
            .get_txs(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Txs for block {} not found in cache", block_num))?;
        Ok((block, txs))
    }

    pub fn load_receipts(&self, block_num: u64) -> anyhow::Result<Vec<crate::data::ReceiptData>> {
        self.cache
            .get_receipts(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Receipts for block {} not found in cache", block_num))
    }

    pub fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    fn build_cfg_env(&self, block_num: u64) -> CfgEnv {
        let spec = spec_id_for_block(self.chain_id, block_num);
        let mut cfg = CfgEnv::new_with_spec(spec);
        cfg.chain_id = self.chain_id;
        cfg.limit_contract_code_size = Some(0x6000);
        cfg
    }

    fn build_block_env(&self, block: &BlockData) -> BlockEnv {
        let spec = spec_id_for_block(self.chain_id, block.number);
        let blob_excess_gas_and_price = if spec >= SpecId::CANCUN {
            Some(BlobExcessGasAndPrice::new_with_spec(0, spec))
        } else {
            None
        };
        BlockEnv {
            number: U256::from(block.number),
            beneficiary: block.coinbase,
            timestamp: U256::from(block.timestamp),
            gas_limit: block.gas_limit,
            basefee: block.base_fee_per_gas.unwrap_or(0) as u64,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::ZERO),
            blob_excess_gas_and_price,
            slot_num: 0,
        }
    }

    fn tx_data_to_tx_env(&self, tx: &TxData) -> TxEnv {
        let kind = match tx.to {
            Some(addr) => TxKind::Call(addr),
            None => TxKind::Create,
        };
        TxEnv {
            tx_type: tx.tx_type,
            caller: tx.from,
            kind,
            value: tx.value,
            data: tx.input.clone(),
            gas_limit: tx.gas_limit,
            gas_price: tx.max_fee_per_gas,
            gas_priority_fee: tx.max_priority_fee_per_gas,
            nonce: tx.nonce,
            access_list: AccessList(
                tx.access_list
                    .iter()
                    .map(|item| AccessListItem {
                        address: item.address,
                        storage_keys: item.slots.to_vec(),
                    })
                    .collect(),
            ),
            chain_id: Some(self.chain_id),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            authorization_list: Vec::new(),
        }
    }

    fn verify_receipt(
        exec: &ExecutionResult,
        receipt: &crate::data::ReceiptData,
        tx_hash: B256,
        block_num: u64,
    ) -> Option<String> {
        let mut mismatches = Vec::new();

        let exec_success = exec.is_success();
        let exec_gas = exec.tx_gas_used();
        let exec_logs = exec.logs();

        if exec_success != receipt.status {
            mismatches.push(format!(
                "status (exec={}, receipt={})",
                exec_success, receipt.status
            ));
        }

        if exec_gas != receipt.gas_used {
            mismatches.push(format!(
                "gas_used (exec={}, receipt={})",
                exec_gas, receipt.gas_used
            ));
        }

        // Polygon adds system-level logs to receipts after EVM execution.
        // Known system addresses: state receiver (0x1001), native token (0x1010).
        const SYSTEM_ADDRS: [Address; 2] = [
            address!("0000000000000000000000000000000000001001"),
            address!("0000000000000000000000000000000000001010"),
        ];
        let receipt_logs: Vec<_> = receipt
            .logs
            .iter()
            .filter(|l| !SYSTEM_ADDRS.contains(&l.address))
            .collect();
        let exec_logs_filtered: Vec<_> = exec_logs
            .iter()
            .filter(|l| !SYSTEM_ADDRS.contains(&l.address))
            .collect();

        if exec_logs_filtered.len() != receipt_logs.len() {
            mismatches.push(format!(
                "log_count (exec={}, receipt={})",
                exec_logs_filtered.len(),
                receipt_logs.len()
            ));
        } else {
            // Use the receipt log order as reference; find matching exec log by address
            for (i, r_log) in receipt_logs.iter().enumerate() {
                let exec_log = exec_logs_filtered.iter().find(|l| l.address == r_log.address);
                match exec_log {
                    None => mismatches.push(format!("log[{}].address not found in exec", i)),
                    Some(l) => {
                        let r_topics = &r_log.topics;
                        let l_topics = l.data.topics();
                        if l_topics.len() != r_topics.len() {
                            mismatches.push(format!(
                                "log[{}].topic_count (exec={}, receipt={})",
                                i,
                                l_topics.len(),
                                r_topics.len()
                            ));
                        } else {
                            for (t, (lt, rt)) in l_topics.iter().zip(r_topics.iter()).enumerate() {
                                if lt != rt {
                                    mismatches.push(format!("log[{}].topic[{}]", i, t));
                                }
                            }
                        }
                    }
                }
            }
        }

        if mismatches.is_empty() {
            return None;
        }

        Some(format!(
            "Block {} tx {}: {}",
            block_num,
            tx_hash,
            mismatches.join(", ")
        ))
    }

    fn build_executed_tx(
        tx: &TxData,
        exec_result: &ExecutionResult,
        receipt: Option<&ReceiptData>,
        block_num: u64,
    ) -> ExecutedTx {
        let mismatch = match receipt {
            Some(r) => Self::verify_receipt(exec_result, r, tx.hash, block_num),
            None => Some("receipt not found".to_string()),
        };
        if let Some(ref msg) = mismatch {
            tracing::warn!("{}", msg);
        }
        let output_bytes = exec_result.output().cloned().unwrap_or_default();
        ExecutedTx {
            tx_hash: tx.hash,
            index: 0,
            status: exec_result.is_success(),
            gas_used: exec_result.tx_gas_used(),
            gas_effective: tx.max_fee_per_gas,
            logs: exec_result
                .logs()
                .iter()
                .map(|l| ExecutedLog {
                    address: l.address,
                    topics: l.data.topics().to_vec(),
                    data: l.data.data.clone(),
                })
                .collect(),
            output: output_bytes,
            error: mismatch,
        }
    }

    /// Replay all transactions in a block up to and including `tx_index`.
    ///
    /// Used primarily by the CLI `replay` command for receipt verification
    /// debugging. Returns the final EVM state snapshot and per-transaction
    /// execution results.
    ///
    /// Receipt verification compares revm execution results (status, gas used,
    /// logs) against cached receipts. Polygon system logs (0x1001, 0x1010)
    /// are filtered before comparison.
    pub fn replay_to(
        &self,
        block_num: u64,
        tx_index: usize,
    ) -> anyhow::Result<(CacheDB<CachedRpcDb>, Vec<ExecutedTx>)> {
        let (_block, txs) = self.load_block_data(block_num)?;
        let receipts = self.load_receipts(block_num)?;

        let actual_tx_count = txs.len();
        let end = tx_index.min(actual_tx_count.saturating_sub(1));

        tracing::info!(
            "Replaying block {} ({} txs) up to tx index {}",
            block_num,
            actual_tx_count,
            end
        );

        let state_block = block_num.saturating_sub(1);
        let inner_db = CachedRpcDb::new(
            self.handle.clone(),
            self.cache.clone(),
            self.rpc.clone(),
            self.chain_id,
            state_block,
        );
        let mut cache_db = CacheDB::new(inner_db);

        if self.chain_id == 137 {
            register_polygon_precompiles(&mut cache_db, block_num)?;
        }

        let cfg_env = self.build_cfg_env(block_num);
        let block_env = self.build_block_env(&_block);

        let ctx = Context::mainnet()
            .with_db(cache_db)
            .with_cfg(cfg_env)
            .with_block(block_env);

        let mut evm = ctx.build_mainnet();
        let mut results = Vec::with_capacity(end + 1);
        let mut total_match = 0u64;
        let mut total_mismatch = 0u64;

        for (i, tx) in txs.iter().enumerate().take(end + 1) {
            let tx_env = self.tx_data_to_tx_env(tx);
            let exec_result = match evm.transact_commit(tx_env) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Block {} tx {} ({}) execution error: {:?}",
                        block_num,
                        i,
                        tx.hash,
                        e
                    );
                    ExecutionResult::Revert {
                        gas: ResultGas::new_with_state_gas(tx.gas_limit, 0, 0, 0),
                        logs: Vec::new(),
                        output: Bytes::new(),
                    }
                }
            };
            let mut executed = Self::build_executed_tx(tx, &exec_result, receipts.get(i), block_num);
            executed.index = i as u64;
            if executed.error.is_some() {
                total_mismatch += 1;
            } else {
                total_match += 1;
            }
            results.push(executed);
        }

        let total = total_match + total_mismatch;
        if total > 0 {
            let pct = (total_match as f64 / total as f64) * 100.0;
            tracing::info!(
                "Receipt verification: {}/{} match ({:.1}%)",
                total_match,
                total,
                pct
            );
        }

        let cache_db = evm.ctx.journaled_state.database.clone();
        drop(evm);
        Ok((cache_db, results))
    }

    /// Replay an entire block (all txs).
    pub fn replay_block(
        &self,
        block_num: u64,
    ) -> anyhow::Result<(CacheDB<CachedRpcDb>, Vec<ExecutedTx>)> {
        let txs = self
            .cache
            .get_txs(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Txs for block {} not found in cache", block_num))?;
        let tx_count = txs.len();
        self.replay_to(block_num, tx_count.saturating_sub(1))
    }

    /// Replay a block tx-by-tx, invoking `on_tx` after each transaction.
    /// Maintains a single EVM context across all txs — efficient for MEV detection.
    ///
    /// The callback receives (tx_index, &ExecutedTx, &CacheDB<CachedRpcDb>).
    pub fn replay_each(
        &self,
        block_num: u64,
        mut on_tx: impl FnMut(usize, &ExecutedTx, &CacheDB<CachedRpcDb>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let (block, txs) = self.load_block_data(block_num)?;
        let receipts = self.load_receipts(block_num)?;

        let state_block = block_num.saturating_sub(1);
        let inner_db = CachedRpcDb::new(
            self.handle.clone(),
            self.cache.clone(),
            self.rpc.clone(),
            self.chain_id,
            state_block,
        );
        let mut cache_db = CacheDB::new(inner_db);

        if self.chain_id == 137 {
            register_polygon_precompiles(&mut cache_db, block_num)?;
        }

        let cfg_env = self.build_cfg_env(block_num);
        let block_env = self.build_block_env(&block);

        let ctx = Context::mainnet()
            .with_db(cache_db)
            .with_cfg(cfg_env)
            .with_block(block_env);

        let mut evm = ctx.build_mainnet();

        for (i, tx) in txs.iter().enumerate() {
            let tx_env = self.tx_data_to_tx_env(tx);
            let exec_result = match evm.transact_commit(tx_env) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Block {} tx {} ({}) execution error: {:?}",
                        block_num,
                        i,
                        tx.hash,
                        e
                    );
                    ExecutionResult::Revert {
                        gas: ResultGas::new_with_state_gas(tx.gas_limit, 0, 0, 0),
                        logs: Vec::new(),
                        output: Bytes::new(),
                    }
                }
            };
            let mut executed = Self::build_executed_tx(tx, &exec_result, receipts.get(i), block_num);
            executed.index = i as u64;
            on_tx(i, &executed, &evm.ctx.journaled_state.database)?;
        }

        Ok(())
    }

    /// Replay a block, skipping EVM execution for transactions that don't
    /// interact with tracked pools or tokens.
    ///
    /// This is the **primary performance optimization** of the backtest engine.
    /// The filter receives `(tx_data, receipt_logs)` and returns `true` if
    /// the transaction touches a tracked address. Transactions that fail the
    /// filter take the fast path: `ExecutedTx` is synthesized from cached
    /// receipt data with no EVM execution and no state changes applied.
    ///
    /// For transactions that pass the filter, full EVM execution proceeds
    /// with receipt verification. After each transaction, `on_tx` is called
    /// with the executed result and the current EVM database state, allowing
    /// the caller to update pool reserves and run detection strategies.
    ///
    /// # Polygon
    /// For chain 137, BLS12-377 precompiles are registered and the EVM spec
    /// is selected based on block number before replay begins.
    pub fn replay_each_filtered(
        &self,
        block_num: u64,
        filter: impl Fn(&TxData, &[LogData]) -> bool,
        mut on_tx: impl FnMut(usize, &ExecutedTx, &CacheDB<CachedRpcDb>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let (block, txs) = self.load_block_data(block_num)?;
        let receipts = self.load_receipts(block_num)?;

        let state_block = block_num.saturating_sub(1);
        let inner_db = CachedRpcDb::new(
            self.handle.clone(),
            self.cache.clone(),
            self.rpc.clone(),
            self.chain_id,
            state_block,
        );
        let mut cache_db = CacheDB::new(inner_db);

        if self.chain_id == 137 {
            register_polygon_precompiles(&mut cache_db, block_num)?;
        }

        let cfg_env = self.build_cfg_env(block_num);
        let block_env = self.build_block_env(&block);

        let ctx = Context::mainnet()
            .with_db(cache_db)
            .with_cfg(cfg_env)
            .with_block(block_env);

        let mut evm = ctx.build_mainnet();

        for (i, tx) in txs.iter().enumerate() {
            let receipt_logs = receipts
                .get(i)
                .map(|r| r.logs.as_slice())
                .unwrap_or_default();

            let mut executed = if filter(tx, receipt_logs) {
                let tx_env = self.tx_data_to_tx_env(tx);
                let exec_result = match evm.transact_commit(tx_env) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(
                            "Block {} tx {} ({}) execution error: {:?}",
                            block_num,
                            i,
                            tx.hash,
                            e
                        );
                        ExecutionResult::Revert {
                            gas: ResultGas::new_with_state_gas(tx.gas_limit, 0, 0, 0),
                            logs: Vec::new(),
                            output: Bytes::new(),
                        }
                    }
                };
                Self::build_executed_tx(tx, &exec_result, receipts.get(i), block_num)
            } else {
                Self::synthesize_tx(tx, receipts.get(i))
            };
            executed.index = i as u64;

            on_tx(i, &executed, &evm.ctx.journaled_state.database)?;
        }

        Ok(())
    }

    /// Build an `ExecutedTx` from cached receipt data without EVM execution.
    /// Used by `replay_each_filtered` for the fast path (non-pool txs).
    fn synthesize_tx(tx: &TxData, receipt: Option<&ReceiptData>) -> ExecutedTx {
        ExecutedTx {
            tx_hash: tx.hash,
            index: 0,
            status: receipt.map(|r| r.status).unwrap_or(false),
            gas_used: receipt.map(|r| r.gas_used).unwrap_or(0),
            gas_effective: tx.max_fee_per_gas,
            logs: receipt
                .map(|r| {
                    r.logs
                        .iter()
                        .map(|l| ExecutedLog {
                            address: l.address,
                            topics: l.topics.clone(),
                            data: l.data.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            output: Bytes::new(),
            error: Some("skipped".to_string()),
        }
    }
}

/// A forkable state snapshot wrapping CacheDB.
pub struct StateSnapshot {
    db: CacheDB<CachedRpcDb>,
}

impl StateSnapshot {
    pub fn new(db: CacheDB<CachedRpcDb>) -> Self {
        StateSnapshot { db }
    }

    pub fn db(&self) -> &CacheDB<CachedRpcDb> {
        &self.db
    }

    pub fn db_mut(&mut self) -> &mut CacheDB<CachedRpcDb> {
        &mut self.db
    }

    /// Create an independent fork of this state.
    /// Writes to the fork do not affect the original.
    pub fn fork(&self) -> Self {
        StateSnapshot {
            db: self.db.clone(),
        }
    }
}

