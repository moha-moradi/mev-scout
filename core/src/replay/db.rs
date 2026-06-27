//! Lazy-fetch EVM database bridging SQLite cache and RPC for revm replays.
//!
//! Provides [`CachedRpcDb`] — a three-tier lookup strategy (in-memory → SQLite
//! → RPC) that implements revm's `Database` and `DatabaseRef` traits. All RPC
//! results are cached back to SQLite for subsequent lookups, making large
//! backtests feasible by only fetching state for addresses touched during
//! execution.

use std::collections::HashMap;
use std::fmt;

use alloy::primitives::{keccak256, Address, B256, U256};
use revm::bytecode::Bytecode;
use revm::database_interface::DBErrorMarker;
use revm::primitives::KECCAK_EMPTY;
use revm::state::AccountInfo;
use revm::{Database, DatabaseRef};

use crate::cache::SqliteStore;
use crate::data::AccountData;
use crate::rpc::RpcClient;

/// Database error for CachedRpcDb.
#[derive(Debug)]
pub struct DbError(pub anyhow::Error);

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

    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
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
