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

/// In-memory caches for a single block replay. Cleared on block transitions.
#[derive(Clone)]
struct CacheState {
    accounts: HashMap<Address, AccountInfo>,
    codes: HashMap<B256, Bytecode>,
    storage: HashMap<(Address, U256), U256>,
    code_hash_to_address: HashMap<B256, Address>,
}

impl CacheState {
    fn new() -> Self {
        CacheState {
            accounts: HashMap::new(),
            codes: HashMap::new(),
            storage: HashMap::new(),
            code_hash_to_address: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.accounts.clear();
        self.codes.clear();
        self.storage.clear();
        self.code_hash_to_address.clear();
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
    cache_state: CacheState,
}

impl Clone for CachedRpcDb {
    fn clone(&self) -> Self {
        CachedRpcDb {
            handle: self.handle.clone(),
            cache: self.cache.clone(),
            rpc: self.rpc.clone(),
            chain_id: self.chain_id,
            block_number: self.block_number,
            cache_state: self.cache_state.clone(),
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
            cache_state: CacheState::new(),
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn set_block_number(&mut self, n: u64) {
        self.block_number = n;
        self.cache_state.clear();
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
        if let Some(info) = self.cache_state.accounts.get(&address) {
            return Ok(Some(info.clone()));
        }
        let result = DatabaseRef::basic_ref(self, address)?;

        if let Some(ref info) = result {
            if info.code_hash != KECCAK_EMPTY
                && !self.cache_state.codes.contains_key(&info.code_hash)
            {
                match &info.code {
                    Some(code) => {
                        self.cache_state.codes.insert(info.code_hash, code.clone());
                    }
                    None => {
                        let code_bytes = {
                            let from_cache = self
                                .cache
                                .get_code(address)
                                .ok()
                                .flatten()
                                .filter(|bytes| keccak256(bytes) == info.code_hash);
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
                        self.cache_state
                            .codes
                            .insert(info.code_hash, Bytecode::new_raw(code_bytes));
                    }
                }
                self.cache_state
                    .code_hash_to_address
                    .insert(info.code_hash, address);
            }

            let code = info.code.clone().or_else(|| {
                self.cache_state.codes.get(&info.code_hash).cloned()
            });

            let full_info = AccountInfo {
                nonce: info.nonce,
                balance: info.balance,
                code,
                code_hash: info.code_hash,
                account_id: None,
            };

            self.cache
                .put_account(
                    self.block_number,
                    address,
                    &AccountData {
                        nonce: info.nonce,
                        balance: info.balance,
                        code_hash: info.code_hash,
                    },
                )
                .map_err(DbError)?;

            self.cache_state
                .accounts
                .insert(address, full_info.clone());
            Ok(Some(full_info))
        } else {
            Ok(None)
        }
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        DatabaseRef::code_by_hash_ref(self, code_hash)
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        if let Some(value) = self.cache_state.storage.get(&(address, index)) {
            return Ok(*value);
        }
        let value = DatabaseRef::storage_ref(self, address, index)?;
        self.cache_state.storage.insert((address, index), value);
        Ok(value)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        DatabaseRef::block_hash_ref(self, number)
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
            let code = self.load_code(address, acct.code_hash)?;
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
        let code = self.load_code(address, code_hash)?;
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
        if let Some(code) = self.cache_state.codes.get(&code_hash) {
            return Ok(code.clone());
        }
        if let Some(&addr) = self.cache_state.code_hash_to_address.get(&code_hash) {
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

impl CachedRpcDb {
    /// Resolve the runtime bytecode for an account. Prefers the SQLite code
    /// cache (verified against the requested code hash) and falls back to an
    /// archive `eth_getCode` RPC call, caching the result.
    ///
    /// Returning `code` directly from `basic_ref` keeps revm from routing
    /// through `code_by_hash`, which cannot resolve a hash without a known
    /// address (`CacheDB` only calls `basic_ref`, never the mutable `basic`).
    fn load_code(&self, address: Address, code_hash: B256) -> Result<Option<Bytecode>, DbError> {
        if code_hash == KECCAK_EMPTY {
            return Ok(None);
        }
        if let Some(bytes) = self
            .cache
            .get_code(address)
            .ok()
            .flatten()
            .filter(|bytes| keccak256(bytes) == code_hash)
        {
            return Ok(Some(Bytecode::new_raw(bytes)));
        }
        let bytes = self
            .block_on_rpc(self.rpc.get_code(address, self.block_number))
            .map_err(DbError)?;
        if !bytes.is_empty() {
            let _ = self.cache.put_code(address, &bytes);
        }
        Ok(Some(Bytecode::new_raw(bytes)))
    }
}
