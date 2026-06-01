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

use crate::cache::CacheStore;
use crate::data::{AccountData, BlockData, ExecutedLog, ExecutedTx, TxData};
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

/// Lazy-fetch database wrapping sled cache and RPC.
/// Implements `Database` for use with revm's `CacheDB`.
pub struct CachedRpcDb {
    handle: tokio::runtime::Handle,
    cache: CacheStore,
    rpc: RpcClient,
    chain_id: u64,
    block_number: u64,
    accounts: HashMap<Address, AccountInfo>,
    codes: HashMap<B256, Bytecode>,
    storage: HashMap<(Address, U256), U256>,
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
        }
    }
}

impl CachedRpcDb {
    pub fn new(
        handle: tokio::runtime::Handle,
        cache: CacheStore,
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
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn set_block_number(&mut self, n: u64) {
        self.block_number = n;
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
                }
            }
            self.accounts.insert(address, info.clone());
            return Ok(Some(info));
        }
        let (nonce, balance, code) = self
            .block_on_rpc(self.rpc.get_account(address, self.block_number))
            .map_err(DbError)?;
        let code_hash = if code.is_empty() {
            KECCAK_EMPTY
        } else {
            keccak256(&code)
        };
        let bytecode = if code.is_empty() {
            Bytecode::new()
        } else {
            Bytecode::new_raw(code.clone())
        };
        let info = AccountInfo {
            nonce,
            balance,
            code: Some(bytecode.clone()),
            code_hash,
            account_id: None,
        };

        if !code.is_empty() {
            self.codes.insert(code_hash, bytecode);
            self.cache
                .put_code(address, &code)
                .map_err(DbError)?;
        }
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
        tracing::warn!(?code_hash, "code_by_hash: unknown code hash");
        Ok(Bytecode::new())
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
        let (nonce, balance, code) = self
            .block_on_rpc(self.rpc.get_account(address, self.block_number))
            .map_err(DbError)?;
        let code_hash = if code.is_empty() {
            KECCAK_EMPTY
        } else {
            keccak256(&code)
        };
        Ok(Some(AccountInfo {
            nonce,
            balance,
            code: if code.is_empty() {
                None
            } else {
                Some(Bytecode::new_raw(code))
            },
            code_hash,
            account_id: None,
        }))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::new());
        }
        tracing::warn!(?code_hash, "code_by_hash_ref: unknown code hash");
        Ok(Bytecode::new())
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
    let rpc: RpcClient;
    let handle: tokio::runtime::Handle;
    let code_09: Bytes;
    let code_0a: Bytes;
    let code_0b: Bytes;
    let code_0c: Bytes;

    {
        let inner = &db.db;
        rpc = inner.rpc.clone();
        handle = inner.handle.clone();
    }

    let block_on = |f| tokio::task::block_in_place(|| handle.block_on(f));
    code_09 = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x09), prev_block))
        .unwrap_or_default();
    code_0a = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0a), prev_block))
        .unwrap_or_default();
    code_0b = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0b), prev_block))
        .unwrap_or_default();
    code_0c = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0c), prev_block))
        .unwrap_or_default();

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
        if let Ok(code) = block_on(rpc.get_code_no_retry(addr, prev_block)) {
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

/// Block replayer that replays historical blocks using revm.
pub struct BlockReplayer {
    handle: tokio::runtime::Handle,
    cache: CacheStore,
    rpc: RpcClient,
    chain_id: u64,
}

impl BlockReplayer {
    pub fn new(
        handle: tokio::runtime::Handle,
        cache: CacheStore,
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
        Ok(self
            .cache
            .get_txs(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Txs for block {} not found in cache", block_num))?)
    }

    fn load_block_data(&self, block_num: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
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

    fn load_receipts(&self, block_num: u64) -> anyhow::Result<Vec<crate::data::ReceiptData>> {
        Ok(self
            .cache
            .get_receipts(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Receipts for block {} not found in cache", block_num))?)
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
                        storage_keys: item.slots.iter().copied().collect(),
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
            for (i, (l, r)) in exec_logs_filtered.iter().zip(receipt_logs.iter()).enumerate() {
                if l.address != r.address {
                    mismatches.push(format!("log[{}].address", i));
                }
                if !l.data.topics().is_empty() && !r.topics.is_empty() {
                    if l.data.topics()[0] != r.topics[0] {
                        mismatches.push(format!("log[{}].topic[0]", i));
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

    /// Replay all txs in a block up to (and including) `tx_index`.
    /// Returns the final state snapshot and execution results.
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
