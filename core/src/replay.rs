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

use std::collections::HashMap;
use std::fmt;

use alloy::primitives::{address, keccak256, Address, B256, Bytes, U256};
use revm::bytecode::Bytecode;
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::result::{ExecutionResult, ResultGas};
use revm::context_interface::transaction::{AccessList, AccessListItem};
use revm::database::CacheDB;
use revm::database_interface::DBErrorMarker;
use revm::handler::{ExecuteCommitEvm, MainBuilder, MainContext};
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::primitives::hardfork::SpecId;
use revm::state::AccountInfo;
use revm::{Context, Database, DatabaseRef};

/// Database error for CachedRpcDb.
#[derive(Debug)]
pub struct DbError(anyhow::Error);

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl core::error::Error for DbError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.0.source()
    }
}

impl DBErrorMarker for DbError {
    fn is_fatal(&self) -> bool {
        false
    }
}

use crate::cache::SqliteStore;
use crate::data::{AccountData, BlockData, ExecutedLog, ExecutedTx, LogData, TxData};
use crate::rpc::RpcClient;

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

/// Lazy-fetch database wrapping SQLite cache and RPC.
///
/// Implements revm's `Database` trait, providing a three-tier lookup strategy:
/// 1. In-memory HashMap (within a single block replay)
/// 2. SQLite cache (persistent, keyed by block number + address/slot)
/// 3. RPC fallback (`eth_getProof`, `eth_getStorageAt`, `eth_getCodeAt`)
///
/// All RPC results are cached back to SQLite for subsequent lookups. This is
/// the mechanism that makes large backtests feasible — the EVM only fetches
/// state for addresses that are actually touched during execution.
///
/// The database operates at a specific `block_number` (the historical block
/// being replayed), but can be updated via `set_block_number()` during
/// cross-block operations.
pub struct CachedRpcDb {
    handle: tokio::runtime::Handle,
    cache: SqliteStore,
    rpc: RpcClient,
    chain_id: u64,
    block_number: u64,
    accounts: HashMap<Address, AccountInfo>,
    codes: HashMap<B256, Bytecode>,
    storage: HashMap<(Address, U256), U256>,
    code_hash_to_address: HashMap<B256, Address>,
}

impl Clone for CachedRpcDb {
    fn clone(&self) -> Self {
        CachedRpcDb {
            handle: self.handle.clone(),
            cache: self.cache.clone(),
            rpc: self.rpc.clone(),
            chain_id: self.chain_id,
            block_number: self.block_number,
            accounts: self.accounts.clone(),
            codes: self.codes.clone(),
            storage: self.storage.clone(),
            code_hash_to_address: self.code_hash_to_address.clone(),
        }
    }
}

impl CachedRpcDb {
    pub fn new(
        handle: tokio::runtime::Handle,
        cache: SqliteStore,
        rpc: RpcClient,
        chain_id: u64,
        block_number: u64,
    ) -> Self {
        CachedRpcDb {
            handle,
            cache,
            rpc,
            chain_id,
            block_number,
            accounts: HashMap::new(),
            codes: HashMap::new(),
            storage: HashMap::new(),
            code_hash_to_address: HashMap::new(),
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn set_block_number(&mut self, n: u64) {
        self.block_number = n;
        // Invalidate all in-memory caches — state is block-dependent.
        self.accounts.clear();
        self.codes.clear();
        self.storage.clear();
        self.code_hash_to_address.clear();
    }

    pub fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    /// Execute an async RPC call, handling nested runtime scenarios.
    fn block_on_rpc<F: std::future::Future<Output = T>, T>(&self, future: F) -> T {
        tokio::task::block_in_place(|| self.handle.block_on(future))
    }
}

impl Database for CachedRpcDb {
    type Error = DbError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(info) = self.accounts.get(&address) {
            return Ok(Some(info.clone()));
        }
        if let Some(acct) = self
            .cache
            .get_account(self.block_number, address)
            .map_err(DbError)?
        {
            let mut info = AccountInfo {
                nonce: acct.nonce,
                balance: acct.balance,
                code: None,
                code_hash: acct.code_hash,
                account_id: None,
            };
            if acct.code_hash != KECCAK_EMPTY {
                if let Some(code) = self.codes.get(&acct.code_hash) {
                    info.code = Some(code.clone());
                } else if let Ok(Some(code_bytes)) = self.cache.get_code(address) {
                    let bytecode = Bytecode::new_raw(code_bytes);
                    info.code = Some(bytecode.clone());
                    self.codes.insert(acct.code_hash, bytecode);
                    self.code_hash_to_address.insert(acct.code_hash, address);
                }
            }
            self.accounts.insert(address, info.clone());
            return Ok(Some(info));
        }
        let (nonce, balance, code_hash, _) = self
            .block_on_rpc(self.rpc.get_proof(address, &[], self.block_number))
            .map_err(DbError)?;

        if code_hash != KECCAK_EMPTY && !self.codes.contains_key(&code_hash) {
            let code_bytes = {
                let from_cache = self
                    .cache
                    .get_code(address)
                    .ok()
                    .flatten()
                    .filter(|bytes| keccak256(bytes) == code_hash);
                match from_cache {
                    Some(bytes) => bytes,
                    None => {
                        let bytes = self
                            .block_on_rpc(self.rpc.get_code(address, self.block_number))
                            .map_err(DbError)?;
                        self.cache.put_code(address, &bytes).map_err(DbError)?;
                        bytes
                    }
                }
            };
            self.codes
                .insert(code_hash, Bytecode::new_raw(code_bytes));
            self.code_hash_to_address.insert(code_hash, address);
        }

        let info = AccountInfo {
            nonce,
            balance,
            code: self.codes.get(&code_hash).cloned(),
            code_hash,
            account_id: None,
        };
        self.cache
            .put_account(
                self.block_number,
                address,
                &AccountData {
                    nonce,
                    balance,
                    code_hash,
                },
            )
            .map_err(DbError)?;
        self.accounts.insert(address, info.clone());
        Ok(Some(info))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::new());
        }
        if let Some(code) = self.codes.get(&code_hash) {
            return Ok(code.clone());
        }
        // Try SQLite lookup via address mapping
        if let Some(&addr) = self.code_hash_to_address.get(&code_hash) {
            if let Ok(Some(code_bytes)) = self.cache.get_code(addr) {
                let bytecode = Bytecode::new_raw(code_bytes);
                self.codes.insert(code_hash, bytecode.clone());
                return Ok(bytecode);
            }
        }
        Err(DbError(anyhow::anyhow!(
            "code_by_hash: unknown code hash {code_hash:?}"
        )))
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        if let Some(value) = self.storage.get(&(address, index)) {
            return Ok(*value);
        }
        if let Some(value) = self
            .cache
            .get_slot(self.block_number, address, index)
            .map_err(DbError)?
        {
            self.storage.insert((address, index), value);
            return Ok(value);
        }
        let value = self
            .block_on_rpc(self.rpc.get_storage_at(address, index, self.block_number))
            .map_err(DbError)?;
        self.cache
            .put_slot(self.block_number, address, index, value)
            .map_err(DbError)?;
        self.storage.insert((address, index), value);
        Ok(value)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        match self.cache.get_block(number).map_err(DbError)? {
            Some(block) => Ok(block.hash),
            None => Ok(B256::ZERO),
        }
    }
}

impl DatabaseRef for CachedRpcDb {
    type Error = DbError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(acct) = self
            .cache
            .get_account(self.block_number, address)
            .map_err(DbError)?
        {
            let code = if acct.code_hash != KECCAK_EMPTY {
                self.cache
                    .get_code(address)
                    .ok()
                    .flatten()
                    .map(Bytecode::new_raw)
            } else {
                None
            };
            return Ok(Some(AccountInfo {
                nonce: acct.nonce,
                balance: acct.balance,
                code,
                code_hash: acct.code_hash,
                account_id: None,
            }));
        }
        let (nonce, balance, code_hash, _) = self
            .block_on_rpc(self.rpc.get_proof(address, &[], self.block_number))
            .map_err(DbError)?;
        let code = if code_hash != KECCAK_EMPTY {
            self.cache
                .get_code(address)
                .ok()
                .flatten()
                .filter(|bytes| keccak256(bytes) == code_hash)
                .map(Bytecode::new_raw)
        } else {
            None
        };
        Ok(Some(AccountInfo {
            nonce,
            balance,
            code,
            code_hash,
            account_id: None,
        }))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::new());
        }
        if let Some(code) = self.codes.get(&code_hash) {
            return Ok(code.clone());
        }
        if let Some(&addr) = self.code_hash_to_address.get(&code_hash) {
            if let Ok(Some(code_bytes)) = self.cache.get_code(addr) {
                return Ok(Bytecode::new_raw(code_bytes));
            }
        }
        Err(DbError(anyhow::anyhow!(
            "code_by_hash_ref: unknown code hash {code_hash:?}"
        )))
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        if let Some(value) = self
            .cache
            .get_slot(self.block_number, address, index)
            .map_err(DbError)?
        {
            return Ok(value);
        }
        self.block_on_rpc(self.rpc.get_storage_at(address, index, self.block_number))
            .map_err(DbError)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        match self.cache.get_block(number).map_err(DbError)? {
            Some(block) => Ok(block.hash),
            None => Ok(B256::ZERO),
        }
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
        (inner.rpc.clone(), inner.handle.clone())
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
            tx_type: 0,
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
        let (block, txs) = self.load_block_data(block_num)?;
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
        let block_env = self.build_block_env(&block);

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

            let receipt = receipts.get(i);
            let mismatch = match receipt {
                Some(r) => Self::verify_receipt(&exec_result, r, tx.hash, block_num),
                None => Some("receipt not found".to_string()),
            };

            if let Some(msg) = &mismatch {
                total_mismatch += 1;
                tracing::warn!("{}", msg);
            } else {
                total_match += 1;
            }

            let output_bytes = exec_result.output().cloned().unwrap_or_default();
            let executed = ExecutedTx {
                tx_hash: tx.hash,
                index: i as u64,
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
                error: mismatch.clone(),
            };
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

            let receipt = receipts.get(i);
            let mismatch = match receipt {
                Some(r) => Self::verify_receipt(&exec_result, r, tx.hash, block_num),
                None => Some("receipt not found".to_string()),
            };

            if let Some(ref msg) = mismatch {
                tracing::warn!("{}", msg);
            }

            let output_bytes = exec_result.output().cloned().unwrap_or_default();
            let executed = ExecutedTx {
                tx_hash: tx.hash,
                index: i as u64,
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
            };

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

            let executed = if filter(tx, receipt_logs) {
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

                let receipt = receipts.get(i);
                let mismatch = match receipt {
                    Some(r) => Self::verify_receipt(&exec_result, r, tx.hash, block_num),
                    None => Some("receipt not found".to_string()),
                };
                if let Some(ref msg) = mismatch {
                    tracing::warn!("{}", msg);
                }

                let output_bytes = exec_result.output().cloned().unwrap_or_default();
                ExecutedTx {
                    tx_hash: tx.hash,
                    index: i as u64,
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
            } else {
                let receipt = receipts.get(i);
                ExecutedTx {
                    tx_hash: tx.hash,
                    index: i as u64,
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
            };

            on_tx(i, &executed, &evm.ctx.journaled_state.database)?;
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Log;
    use crate::data::ReceiptData;
    use revm::context_interface::result::{Output, SuccessReason};
    use revm::primitives::hardfork::SpecId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REPLAY_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_cache() -> SqliteStore {
        let id = REPLAY_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("mev_ut_replay_{}.sqlite", id));
        let _ = std::fs::remove_file(&path);
        SqliteStore::open(&path, 137).unwrap()
    }

    fn dummy_rpc() -> RpcClient {
        RpcClient::new("http://localhost:9999", 1).unwrap()
    }

    // --- spec_id_for_block ---

    #[test]
    fn test_spec_id_polygon_cancun() {
        assert_eq!(super::spec_id_for_block(137, 50_523_000), SpecId::CANCUN);
        assert_eq!(super::spec_id_for_block(137, 100_000_000), SpecId::CANCUN);
    }

    #[test]
    fn test_spec_id_polygon_london() {
        assert_eq!(super::spec_id_for_block(137, 23_850_000), SpecId::LONDON);
        assert_eq!(super::spec_id_for_block(137, 40_000_000), SpecId::LONDON);
    }

    #[test]
    fn test_spec_id_polygon_berlin() {
        assert_eq!(super::spec_id_for_block(137, 0), SpecId::BERLIN);
        assert_eq!(super::spec_id_for_block(137, 10_000_000), SpecId::BERLIN);
    }

    #[test]
    fn test_spec_id_other_chain() {
        assert_eq!(super::spec_id_for_block(1, 0), SpecId::NEXT);
        assert_eq!(super::spec_id_for_block(56, 100_000_000), SpecId::NEXT);
    }

    // --- DbError ---

    #[test]
    fn test_db_error_display() {
        let e = DbError(anyhow::anyhow!("test error"));
        assert_eq!(e.to_string(), "test error");
    }

    #[test]
    fn test_db_error_is_fatal() {
        let e = DbError(anyhow::anyhow!("test"));
        assert!(!e.is_fatal());
    }

    #[test]
    fn test_db_error_source_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let e = DbError(anyhow::Error::from(io_err));
        assert!(e.0.source().is_none());
    }

    // --- verify_receipt ---

    fn make_log(addr: Address, topic: B256) -> Log {
        Log::new_unchecked(addr, vec![topic], Bytes::new())
    }

    fn make_receipt_data(status: bool, gas_used: u64, logs: Vec<crate::data::LogData>) -> ReceiptData {
        ReceiptData {
            tx_hash: B256::from([1u8; 32]),
            tx_index: 0,
            status,
            gas_used,
            cumulative_gas_used: gas_used,
            logs,
            contract_address: None,
        }
    }

    fn make_success_exec(gas: u64, logs: Vec<Log>) -> ExecutionResult {
        ExecutionResult::Success {
            reason: SuccessReason::Return,
            gas: ResultGas::new_with_state_gas(gas, 0, 0, 0),
            logs,
            output: Output::Call(Bytes::new()),
        }
    }

    fn make_revert_exec(gas: u64, logs: Vec<Log>) -> ExecutionResult {
        ExecutionResult::Revert {
            gas: ResultGas::new_with_state_gas(gas, 0, 0, 0),
            logs,
            output: Bytes::new(),
        }
    }

    #[test]
    fn test_verify_receipt_success_match() {
        let exec = make_success_exec(21000, vec![]);
        let receipt = make_receipt_data(true, 21000, vec![]);
        assert!(BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1).is_none());
    }

    #[test]
    fn test_verify_receipt_status_mismatch() {
        let exec = make_success_exec(21000, vec![]);
        let receipt = make_receipt_data(false, 21000, vec![]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("status"));
    }

    #[test]
    fn test_verify_receipt_gas_mismatch() {
        let exec = make_success_exec(21000, vec![]);
        let receipt = make_receipt_data(true, 30000, vec![]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("gas_used"));
    }

    #[test]
    fn test_verify_receipt_revert_vs_success() {
        let exec = make_revert_exec(21000, vec![]);
        let receipt = make_receipt_data(true, 21000, vec![]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("status"));
    }

    #[test]
    fn test_verify_receipt_log_count_mismatch() {
        let exec = make_success_exec(21000, vec![make_log(address!("0000000000000000000000000000000000000001"), B256::ZERO)]);
        let receipt = make_receipt_data(true, 21000, vec![]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("log_count"));
    }

    #[test]
    fn test_verify_receipt_log_address_mismatch() {
        let log = make_log(address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), B256::ZERO);
        let rlog = crate::data::LogData { address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"), topics: vec![B256::ZERO], data: Bytes::new() };
        let exec = make_success_exec(21000, vec![log]);
        let receipt = make_receipt_data(true, 21000, vec![rlog]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("log[0].address"));
    }

    #[test]
    fn test_verify_receipt_log_topic_mismatch() {
        let log = make_log(address!("0000000000000000000000000000000000000001"), B256::from([1u8; 32]));
        let rlog = crate::data::LogData { address: address!("0000000000000000000000000000000000000001"), topics: vec![B256::from([2u8; 32])], data: Bytes::new() };
        let exec = make_success_exec(21000, vec![log]);
        let receipt = make_receipt_data(true, 21000, vec![rlog]);
        let result = BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("log[0].topic[0]"));
    }

    #[test]
    fn test_verify_receipt_system_logs_filtered() {
        let system_addr = address!("0000000000000000000000000000000000001001");
        let log = make_log(system_addr, B256::ZERO);
        let exec = make_success_exec(21000, vec![log]);
        let rlog = crate::data::LogData { address: system_addr, topics: vec![B256::ZERO], data: Bytes::new() };
        let receipt = make_receipt_data(true, 21000, vec![rlog]);
        assert!(BlockReplayer::verify_receipt(&exec, &receipt, B256::ZERO, 1).is_none());
    }

    // --- CachedRpcDb ---

    #[test]
    fn test_cached_rpc_db_new() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.handle().clone();
        let cache = temp_cache();
        let rpc = RpcClient::new("http://localhost:9999", 137).unwrap();
        let db = CachedRpcDb::new(handle, cache, rpc, 137, 42);
        assert_eq!(db.block_number(), 42);
        assert_eq!(db.rpc().chain_id(), 137);
    }

    #[test]
    fn test_cached_rpc_db_set_block_number() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.handle().clone();
        let cache = temp_cache();
        let rpc = dummy_rpc();
        let mut db = CachedRpcDb::new(handle, cache, rpc, 1, 42);
        db.set_block_number(100);
        assert_eq!(db.block_number(), 100);
    }

    // --- StateSnapshot ---

    #[test]
    fn test_state_snapshot_new_and_db() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.handle().clone();
        let cache = temp_cache();
        let rpc = dummy_rpc();
        let inner = CachedRpcDb::new(handle, cache, rpc, 1, 42);
        let cache_db = CacheDB::new(inner);
        let snap = StateSnapshot::new(cache_db);
        let db_ref = snap.db();
        assert_eq!(db_ref.db.block_number(), 42);
    }

    #[test]
    fn test_state_snapshot_fork() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.handle().clone();
        let cache = temp_cache();
        let rpc = dummy_rpc();
        let inner = CachedRpcDb::new(handle, cache, rpc, 1, 42);
        let cache_db = CacheDB::new(inner);
        let snap = StateSnapshot::new(cache_db);
        let fork = snap.fork();
        assert_eq!(fork.db().db.block_number(), 42);
    }

    #[test]
    fn test_state_snapshot_db_mut() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.handle().clone();
        let cache = temp_cache();
        let rpc = dummy_rpc();
        let inner = CachedRpcDb::new(handle, cache, rpc, 1, 42);
        let cache_db = CacheDB::new(inner);
        let mut snap = StateSnapshot::new(cache_db);
        snap.db_mut().db.set_block_number(99);
        assert_eq!(snap.db().db.block_number(), 99);
    }
}
