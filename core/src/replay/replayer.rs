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
//!
//! ## Chain schedules
//! [`spec_id_for_header`] maps a block header to the chain's real EVM fork
//! (Ethereum and Polygon by block; BSC and Avalanche by block/timestamp). L2s
//! and unknown chains use the latest deployed spec. See
//! [`register_polygon_precompiles`] for Polygon-specific state setup.
//!
//! ## Receipt verification
//! After each transaction, `verify_receipt` compares the revm execution
//! result against cached receipts (status, gas used, logs). Polygon system
//! logs from `0x1001` and `0x1010` are filtered during comparison.
/// Build a `MainnetEvm` for the given block, handling DB creation and Polygon setup.
macro_rules! build_mainnet_evm {
    ($self:expr, $block_num:expr, $block:expr) => {{
        let cache_db = $self.create_db_for_block($block_num)?;
        let cfg_env = $self.build_cfg_env($block_num, $block.timestamp, $block.mix_hash);
        let block_env = $self.build_block_env($block);
        Context::mainnet()
            .with_db(cache_db)
            .with_cfg(cfg_env)
            .with_block(block_env)
            .build_mainnet()
    }};
}

use alloy::eips::eip7702::{Authorization, SignedAuthorization};
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy::signers::Either;
use revm::bytecode::Bytecode;
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::result::{EVMError, ExecutionResult, InvalidTransaction, ResultGas};
use revm::context_interface::transaction::{AccessList, AccessListItem};
use revm::database::CacheDB;
use revm::handler::{ExecuteCommitEvm, MainBuilder, MainContext};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::state::AccountInfo;
use revm::Context;

use crate::cache::SqliteStore;
use crate::data::{BlockData, ExecutedLog, ExecutedTx, LogData, ReceiptData, TxData};
use crate::rpc::RpcClient;

pub use super::db::{CachedRpcDb, DbError};

/// Polygon BLS12-377 precompile addresses (Heimdall fork)
const BLS12_377_ADDRESSES: [u8; 4] = [0x09, 0x0a, 0x0b, 0x0c];
/// Polygon state receiver system contract
const STATE_RECEIVER: Address = address!("0000000000000000000000000000000000001001");

fn addr_from_last_byte(b: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = b;
    Address::from(bytes)
}

/// Latest *deployed* EVM spec in revm. `SpecId::NEXT` is Amsterdam, whose
/// EIP-8037 state-gas rules no supported chain has activated.
pub const LATEST_LIVE_SPEC: SpecId = SpecId::OSAKA;

/// Select the EVM spec for a block header.
///
/// Ethereum and Polygon activate by block number. BSC and Avalanche activate
/// their later forks by timestamp, so replay paths pass the header timestamp
/// here. Pass `u64::MAX` when only the block number is known; time-based chains
/// then use their latest supported spec.
///
/// Ethereum mainnet block activations:
/// - Frontier: 0
/// - Homestead: 1,150,000
/// - Tangerine Whistle: 2,463,000
/// - Spurious Dragon: 2,675,000
/// - Byzantium: 4,370,000
/// - Petersburg (Constantinople fixes): 7,280,000
/// - Istanbul: 9,069,000
/// - Berlin: 12,244,000
/// - London: 12,965,000
/// - Merge: 15,537,394
/// - Shanghai: 17,034,870
/// - Cancun: 19,426,587
/// - Prague: 22,431,084
/// - Osaka: 23,935,694 (Amsterdam is not scheduled; use the latest live fork)
///
/// Polygon (Bor) block activations:
/// - Petersburg: 0
/// - Istanbul: 3,395,000
/// - Berlin: 14,750,000
/// - London: 23,850,000
/// - Shanghai: 50,523,000
/// - Cancun: 54,876,000
/// - Prague: 73,440,256
///
/// BSC block/timestamp activations:
/// - Istanbul: 0
/// - Berlin + London: block 31,302,048
/// - Shanghai: time 1,705,996,800
/// - Cancun: time 1,718,863,500
/// - Prague: time 1,742,436,600
/// - Osaka: time 1,777,343,400
///
/// Avalanche C-Chain block/timestamp activations:
/// - Istanbul: 0
/// - Berlin: block 1,640,340
/// - London: block 3,308,552
/// - Shanghai (Durango): time 1,709,740,800
/// - Cancun (Etna): time 1,734,368,400
///
/// Arbitrum One (ArbOS version in `mixHash[16..24]`):
/// - Merge: version < 11
/// - Shanghai: version >= 11
/// - Cancun: version >= 20
/// - Prague: version >= 40
/// - Osaka: version >= 50
///
/// OP Mainnet timestamps:
/// - Canyon: 1,704,992,401 (Shanghai)
/// - Ecotone: 1,710,374,401 (Cancun)
/// - Isthmus: 1,746,806,401 (Prague)
///
/// Arbitrum, Base, and OP Mainnet follow their own L2 upgrade schedules
/// (ArbOS / OP Stack), which do not map onto Ethereum block heights. Arbitrum
/// encodes its ArbOS version in `mixHash[16..24]`; OP uses the timestamps
/// recorded in the OP Stack config. Base has its own activation schedule and
/// uses the latest live spec here. Unknown chains do the same.
pub fn spec_id_for_header(
    chain_id: u64,
    block_number: u64,
    timestamp: u64,
    mix_hash: B256,
) -> SpecId {
    match chain_id {
        1 => ethereum_spec_id(block_number),
        137 => polygon_spec_id(block_number),
        56 => bsc_spec_id(block_number, timestamp),
        43114 => avalanche_spec_id(block_number, timestamp),
        42161 => arbitrum_spec_id(mix_hash),
        10 => op_spec_id(timestamp),
        _ => LATEST_LIVE_SPEC,
    }
}

/// Block-only convenience. Time-based chains (BSC/Avalanche/OP) and
/// header-gated chains (Arbitrum) return their latest supported spec because
/// the header is unknown; replay paths should call [`spec_id_for_header`].
pub fn spec_id_for_block(chain_id: u64, block_number: u64) -> SpecId {
    spec_id_for_header(chain_id, block_number, u64::MAX, B256::ZERO)
}

fn ethereum_spec_id(block_number: u64) -> SpecId {
    if block_number >= 23_935_694 {
        SpecId::OSAKA
    } else if block_number >= 22_431_084 {
        SpecId::PRAGUE
    } else if block_number >= 19_426_587 {
        SpecId::CANCUN
    } else if block_number >= 17_034_870 {
        SpecId::SHANGHAI
    } else if block_number >= 15_537_394 {
        SpecId::MERGE
    } else if block_number >= 12_965_000 {
        SpecId::LONDON
    } else if block_number >= 12_244_000 {
        SpecId::BERLIN
    } else if block_number >= 9_069_000 {
        SpecId::ISTANBUL
    } else if block_number >= 7_280_000 {
        SpecId::PETERSBURG
    } else if block_number >= 4_370_000 {
        SpecId::BYZANTIUM
    } else if block_number >= 2_675_000 {
        SpecId::SPURIOUS_DRAGON
    } else if block_number >= 2_463_000 {
        SpecId::TANGERINE
    } else if block_number >= 1_150_000 {
        SpecId::HOMESTEAD
    } else {
        SpecId::FRONTIER
    }
}

fn polygon_spec_id(block_number: u64) -> SpecId {
    if block_number >= 73_440_256 {
        SpecId::PRAGUE
    } else if block_number >= 54_876_000 {
        SpecId::CANCUN
    } else if block_number >= 50_523_000 {
        SpecId::SHANGHAI
    } else if block_number >= 23_850_000 {
        SpecId::LONDON
    } else if block_number >= 14_750_000 {
        SpecId::BERLIN
    } else if block_number >= 3_395_000 {
        SpecId::ISTANBUL
    } else {
        SpecId::PETERSBURG
    }
}

fn bsc_spec_id(block_number: u64, timestamp: u64) -> SpecId {
    if block_number < 31_302_048 {
        return SpecId::ISTANBUL;
    }
    if timestamp == u64::MAX {
        return LATEST_LIVE_SPEC;
    }
    if timestamp >= 1_777_343_400 {
        SpecId::OSAKA
    } else if timestamp >= 1_742_436_600 {
        SpecId::PRAGUE
    } else if timestamp >= 1_718_863_500 {
        SpecId::CANCUN
    } else if timestamp >= 1_705_996_800 {
        SpecId::SHANGHAI
    } else {
        SpecId::LONDON
    }
}

fn avalanche_spec_id(block_number: u64, timestamp: u64) -> SpecId {
    if block_number < 1_640_340 {
        return SpecId::ISTANBUL;
    }
    if block_number < 3_308_552 {
        return SpecId::BERLIN;
    }
    if timestamp == u64::MAX {
        return SpecId::CANCUN;
    }
    if timestamp >= 1_734_368_400 {
        SpecId::CANCUN
    } else if timestamp >= 1_709_740_800 {
        SpecId::SHANGHAI
    } else {
        SpecId::LONDON
    }
}

/// Arbitrum stores its ArbOS version in `mixHash[16..24]` (big-endian u64).
/// A zero version means the caller had no header (block-only convenience), so
/// use the latest live spec instead of regressing to Merge.
fn arbitrum_spec_id(mix_hash: B256) -> SpecId {
    let bytes = mix_hash.as_slice();
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[16..24]);
    let version = u64::from_be_bytes(word);
    if version == 0 {
        LATEST_LIVE_SPEC
    } else if version >= 50 {
        SpecId::OSAKA
    } else if version >= 40 {
        SpecId::PRAGUE
    } else if version >= 20 {
        SpecId::CANCUN
    } else if version >= 11 {
        SpecId::SHANGHAI
    } else {
        SpecId::MERGE
    }
}

fn op_spec_id(timestamp: u64) -> SpecId {
    if timestamp == u64::MAX {
        return LATEST_LIVE_SPEC;
    }
    if timestamp >= 1_746_806_401 {
        SpecId::PRAGUE
    } else if timestamp >= 1_710_374_401 {
        SpecId::CANCUN
    } else if timestamp >= 1_704_992_401 {
        SpecId::SHANGHAI
    } else {
        SpecId::MERGE
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

    let probe_addrs: Vec<u8> = (0x01u8..=0x1f)
        .filter(|b| !BLS12_377_ADDRESSES.contains(b))
        .collect();
    let probe_futures: Vec<_> = probe_addrs
        .iter()
        .map(|b| rpc.get_code_no_retry(addr_from_last_byte(*b), prev_block))
        .collect();
    let probe_codes =
        tokio::task::block_in_place(|| handle.block_on(futures::future::join_all(probe_futures)));
    for (b, code) in probe_addrs.iter().zip(probe_codes) {
        if let Ok(code) = code {
            if !code.is_empty() {
                tracing::warn!(
                    "Unrecognised non-empty contract at precompile-range address {} ({} bytes)",
                    addr_from_last_byte(*b),
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
/// - `replay_to` — replay up to a specific tx index (used by CLI `replay`)
/// - `replay_each_filtered` — replay with a filter to skip non-pool txs
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

/// Outcome of executing a single transaction through revm.
enum ExecOutcome {
    /// Transaction executed (success, revert, or halt).
    Executed(ExecutionResult),
    /// The historical state layer failed (e.g. no archive RPC reachable).
    /// The caller decides whether to synthesize a result from the cached
    /// receipt and continue, or to abort the replay.
    StateLoadFailed(String),
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
            .ok_or_else(|| anyhow::anyhow!("txs for block {} not found in cache", block_num))
    }

    pub fn load_block_data(&self, block_num: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block = self
            .cache
            .get_block(block_num)?
            .ok_or_else(|| anyhow::anyhow!("block {} not found in cache", block_num))?;
        let txs = self
            .cache
            .get_txs(block_num)?
            .ok_or_else(|| anyhow::anyhow!("Txs for block {} not found in cache", block_num))?;
        Ok((block, txs))
    }

    pub fn load_receipts(&self, block_num: u64) -> anyhow::Result<Vec<crate::data::ReceiptData>> {
        self.cache
            .get_receipts(block_num)?
            .ok_or_else(|| anyhow::anyhow!("receipts for block {} not found in cache", block_num))
    }

    pub fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    /// Whether this block's data is already in the local cache.
    ///
    /// Used to tell "block absent because nothing was fetched for it"
    /// (expected when pool discovery narrowed the fetch set) apart from
    /// "block present but replay failed" (a real error).
    pub fn has_cached_block(&self, block_num: u64) -> anyhow::Result<bool> {
        self.cache.has_block(block_num)
    }

    pub(crate) fn build_cfg_env(&self, block_num: u64, timestamp: u64, mix_hash: B256) -> CfgEnv {
        let spec = spec_id_for_header(self.chain_id, block_num, timestamp, mix_hash);
        let mut cfg = CfgEnv::new_with_spec(spec);
        cfg.chain_id = self.chain_id;
        cfg.limit_contract_code_size = Some(0x6000);
        cfg
    }

    pub(crate) fn build_block_env(&self, block: &BlockData) -> BlockEnv {
        let spec = spec_id_for_header(self.chain_id, block.number, block.timestamp, block.mix_hash);
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
            difficulty: block.difficulty,
            prevrandao: Some(block.mix_hash),
            blob_excess_gas_and_price,
            slot_num: 0,
        }
    }

    /// Next-block env for pending inclusion: N+1, timestamp+12, basefee×1.125.
    pub(crate) fn build_next_block_env(&self, prev: &BlockData) -> BlockEnv {
        let next_num = prev.number.saturating_add(1);
        let next_ts = prev.timestamp.saturating_add(12);
        let spec = spec_id_for_header(self.chain_id, next_num, next_ts, prev.mix_hash);
        let blob_excess_gas_and_price = if spec >= SpecId::CANCUN {
            Some(BlobExcessGasAndPrice::new_with_spec(0, spec))
        } else {
            None
        };
        let base = prev.base_fee_per_gas.unwrap_or(0);
        let next_base = base.saturating_mul(9) / 8; // ×1.125
        BlockEnv {
            number: U256::from(next_num),
            beneficiary: prev.coinbase,
            timestamp: U256::from(next_ts),
            gas_limit: prev.gas_limit,
            basefee: next_base as u64,
            difficulty: prev.difficulty,
            prevrandao: Some(prev.mix_hash),
            blob_excess_gas_and_price,
            slot_num: 0,
        }
    }

    pub(crate) fn tx_data_to_tx_env(&self, tx: &TxData) -> TxEnv {
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
            gas_price: tx.gas_price.unwrap_or(tx.max_fee_per_gas),
            gas_priority_fee: tx.max_priority_fee_per_gas.or(tx.gas_price),
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
            authorization_list: tx
                .authorization_list
                .iter()
                .map(|a| {
                    Either::Left(SignedAuthorization::new_unchecked(
                        Authorization {
                            chain_id: a.chain_id,
                            address: a.address,
                            nonce: a.nonce,
                        },
                        a.y_parity,
                        a.r,
                        a.s,
                    ))
                })
                .collect(),
        }
    }

    /// Map a `transact_commit` result to an `ExecOutcome`.
    ///
    /// A `Database` error means the historical state layer failed (e.g. no
    /// archive RPC, RPC outage) — replay results would be invalid for every
    /// transaction, so callers must decide how to handle it (abort, or degrade
    /// to receipt synthesis for verification-style replays). All other errors
    /// (invalid tx, halts, custom) are per-transaction outcomes and are
    /// surfaced as a synthetic revert for receipt comparison.
    fn exec_or_revert(
        block_num: u64,
        tx_index: usize,
        tx_hash: B256,
        tx_gas_limit: u64,
        result: Result<ExecutionResult, EVMError<DbError, InvalidTransaction>>,
    ) -> ExecOutcome {
        match result {
            Ok(r) => ExecOutcome::Executed(r),
            Err(EVMError::Database(db_err)) => {
                tracing::warn!(
                    "Block {block_num} tx {tx_index} ({tx_hash}) state load failed: {db_err}"
                );
                ExecOutcome::StateLoadFailed(format!("{db_err}"))
            }
            Err(e) => {
                tracing::warn!(
                    "Block {block_num} tx {tx_index} ({tx_hash}) execution error: {:?}",
                    e
                );
                ExecOutcome::Executed(ExecutionResult::Revert {
                    gas: ResultGas::new_with_state_gas(tx_gas_limit, 0, 0, 0),
                    logs: Vec::new(),
                    output: Bytes::new(),
                })
            }
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
            // EVM emits logs in execution order, so the sequences must line up positionally.
            for (i, (l, r_log)) in exec_logs_filtered
                .iter()
                .zip(receipt_logs.iter())
                .enumerate()
            {
                if l.address != r_log.address {
                    mismatches.push(format!(
                        "log[{}].address (exec={}, receipt={})",
                        i, l.address, r_log.address
                    ));
                }
                if l.data.data != r_log.data {
                    mismatches.push(format!("log[{}].data", i));
                }
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
        // Prefer receipt.gasUsed over EVM execution gas when available,
        // as eth_estimateGas often overestimates vs actual on-chain consumption.
        let gas_used = receipt
            .map(|r| r.gas_used)
            .unwrap_or_else(|| exec_result.tx_gas_used());
        ExecutedTx {
            tx_hash: tx.hash,
            index: 0,
            status: exec_result.is_success(),
            gas_used,
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

        let mut evm = build_mainnet_evm!(self, block_num, &_block);
        let mut results = Vec::with_capacity(end + 1);
        let mut total_match = 0u64;
        let mut total_mismatch = 0u64;

        for (i, tx) in txs.iter().enumerate().take(end + 1) {
            let tx_env = self.tx_data_to_tx_env(tx);
            let mut executed = match Self::exec_or_revert(
                block_num,
                i,
                tx.hash,
                tx.gas_limit,
                evm.transact_commit(tx_env),
            ) {
                ExecOutcome::Executed(exec_result) => {
                    Self::build_executed_tx(tx, &exec_result, receipts.get(i), block_num)
                }
                ExecOutcome::StateLoadFailed(err_msg) => {
                    Self::synthesize_tx(tx, receipts.get(i), &err_msg)
                }
            };
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

        let mut evm = build_mainnet_evm!(self, block_num, &block);

        for (i, tx) in txs.iter().enumerate() {
            let receipt_logs = receipts
                .get(i)
                .map(|r| r.logs.as_slice())
                .unwrap_or_default();

            let mut executed = if filter(tx, receipt_logs) {
                let tx_env = self.tx_data_to_tx_env(tx);
                match Self::exec_or_revert(
                    block_num,
                    i,
                    tx.hash,
                    tx.gas_limit,
                    evm.transact_commit(tx_env),
                ) {
                    ExecOutcome::Executed(exec_result) => {
                        Self::build_executed_tx(tx, &exec_result, receipts.get(i), block_num)
                    }
                    ExecOutcome::StateLoadFailed(err_msg) => {
                        Self::synthesize_tx(tx, receipts.get(i), &err_msg)
                    }
                }
            } else {
                Self::synthesize_tx(tx, receipts.get(i), "skipped")
            };
            executed.index = i as u64;

            on_tx(i, &executed, &evm.ctx.journaled_state.database)?;
        }

        Ok(())
    }

    /// Create a fresh `CacheDB<CachedRpcDb>` for replaying `block_num`.
    /// Registers Polygon precompiles when chain_id == 137.
    pub(crate) fn create_db_for_block(
        &self,
        block_num: u64,
    ) -> anyhow::Result<CacheDB<CachedRpcDb>> {
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
        Ok(cache_db)
    }

    /// Build an `ExecutedTx` from cached receipt data without EVM execution.
    /// Used by `replay_each_filtered` for the fast path (non-pool txs) and by
    /// verification replays when state could not be loaded. `error_msg` is
    /// surfaced as the per-tx mismatch reason (e.g. `"skipped"`).
    fn synthesize_tx(tx: &TxData, receipt: Option<&ReceiptData>, error_msg: &str) -> ExecutedTx {
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
            error: Some(error_msg.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ethereum_mainnet_uses_its_fork_schedule() {
        let cases = [
            (0, SpecId::FRONTIER),
            (1_149_999, SpecId::FRONTIER),
            (1_150_000, SpecId::HOMESTEAD),
            (2_462_999, SpecId::HOMESTEAD),
            (2_463_000, SpecId::TANGERINE),
            (2_674_999, SpecId::TANGERINE),
            (2_675_000, SpecId::SPURIOUS_DRAGON),
            (4_369_999, SpecId::SPURIOUS_DRAGON),
            (4_370_000, SpecId::BYZANTIUM),
            (7_279_999, SpecId::BYZANTIUM),
            (7_280_000, SpecId::PETERSBURG),
            (9_068_999, SpecId::PETERSBURG),
            (9_069_000, SpecId::ISTANBUL),
            (12_243_999, SpecId::ISTANBUL),
            (12_244_000, SpecId::BERLIN),
            (12_964_999, SpecId::BERLIN),
            (12_965_000, SpecId::LONDON),
            (15_537_393, SpecId::LONDON),
            (15_537_394, SpecId::MERGE),
            (17_034_869, SpecId::MERGE),
            (17_034_870, SpecId::SHANGHAI),
            (19_426_586, SpecId::SHANGHAI),
            (19_426_587, SpecId::CANCUN),
            (22_431_083, SpecId::CANCUN),
            (22_431_084, SpecId::PRAGUE),
            (23_935_693, SpecId::PRAGUE),
            (23_935_694, SpecId::OSAKA),
            (u64::MAX, SpecId::OSAKA),
        ];
        for (block, expected) in cases {
            assert_eq!(
                spec_id_for_block(1, block),
                expected,
                "Ethereum block {block}"
            );
        }
    }

    #[test]
    fn polygon_uses_the_bor_schedule() {
        let cases = [
            (0, SpecId::PETERSBURG),
            (3_394_999, SpecId::PETERSBURG),
            (3_395_000, SpecId::ISTANBUL),
            (14_749_999, SpecId::ISTANBUL),
            (14_750_000, SpecId::BERLIN),
            (23_849_999, SpecId::BERLIN),
            (23_850_000, SpecId::LONDON),
            (50_522_999, SpecId::LONDON),
            (50_523_000, SpecId::SHANGHAI),
            (54_875_999, SpecId::SHANGHAI),
            (54_876_000, SpecId::CANCUN),
            (73_440_255, SpecId::CANCUN),
            (73_440_256, SpecId::PRAGUE),
        ];
        for (block, expected) in cases {
            assert_eq!(
                spec_id_for_block(137, block),
                expected,
                "Polygon block {block}"
            );
        }
    }

    #[test]
    fn bsc_uses_block_then_time_activations() {
        assert_eq!(
            spec_id_for_header(56, 31_302_047, 1_700_000_000, B256::ZERO),
            SpecId::ISTANBUL
        );
        assert_eq!(
            spec_id_for_header(56, 31_302_048, 1_700_000_000, B256::ZERO),
            SpecId::LONDON
        );
        assert_eq!(
            spec_id_for_header(56, 31_302_048, 1_705_996_800, B256::ZERO),
            SpecId::SHANGHAI
        );
        assert_eq!(
            spec_id_for_header(56, 31_302_048, 1_718_863_500, B256::ZERO),
            SpecId::CANCUN
        );
        assert_eq!(
            spec_id_for_header(56, 31_302_048, 1_742_436_600, B256::ZERO),
            SpecId::PRAGUE
        );
        assert_eq!(
            spec_id_for_header(56, 31_302_048, 1_777_343_400, B256::ZERO),
            SpecId::OSAKA
        );
        // Block-only callers get the latest supported spec.
        assert_eq!(spec_id_for_block(56, 40_000_000), LATEST_LIVE_SPEC);
    }

    #[test]
    fn avalanche_uses_block_then_time_activations() {
        assert_eq!(
            spec_id_for_header(43114, 1_640_339, 1_700_000_000, B256::ZERO),
            SpecId::ISTANBUL
        );
        assert_eq!(
            spec_id_for_header(43114, 1_640_340, 1_700_000_000, B256::ZERO),
            SpecId::BERLIN
        );
        assert_eq!(
            spec_id_for_header(43114, 3_308_551, 1_700_000_000, B256::ZERO),
            SpecId::BERLIN
        );
        assert_eq!(
            spec_id_for_header(43114, 3_308_552, 1_700_000_000, B256::ZERO),
            SpecId::LONDON
        );
        assert_eq!(
            spec_id_for_header(43114, 3_308_552, 1_709_740_800, B256::ZERO),
            SpecId::SHANGHAI
        );
        assert_eq!(
            spec_id_for_header(43114, 3_308_552, 1_734_368_400, B256::ZERO),
            SpecId::CANCUN
        );
        assert_eq!(spec_id_for_block(43114, 100_000_000), SpecId::CANCUN);
    }

    fn arb_mix(version: u64) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[16..24].copy_from_slice(&version.to_be_bytes());
        B256::from(bytes)
    }

    #[test]
    fn arbitrum_uses_arbos_version_from_mix_hash() {
        assert_eq!(spec_id_for_header(42161, 1, 0, arb_mix(10)), SpecId::MERGE);
        assert_eq!(
            spec_id_for_header(42161, 1, 0, arb_mix(11)),
            SpecId::SHANGHAI
        );
        assert_eq!(spec_id_for_header(42161, 1, 0, arb_mix(20)), SpecId::CANCUN);
        assert_eq!(spec_id_for_header(42161, 1, 0, arb_mix(40)), SpecId::PRAGUE);
        assert_eq!(spec_id_for_header(42161, 1, 0, arb_mix(50)), SpecId::OSAKA);
        // No header available: latest live spec.
        assert_eq!(spec_id_for_block(42161, 1), LATEST_LIVE_SPEC);
    }

    #[test]
    fn op_mainnet_uses_timestamp_activations() {
        assert_eq!(
            spec_id_for_header(10, 1, 1_704_992_400, B256::ZERO),
            SpecId::MERGE
        );
        assert_eq!(
            spec_id_for_header(10, 1, 1_704_992_401, B256::ZERO),
            SpecId::SHANGHAI
        );
        assert_eq!(
            spec_id_for_header(10, 1, 1_710_374_401, B256::ZERO),
            SpecId::CANCUN
        );
        assert_eq!(
            spec_id_for_header(10, 1, 1_746_806_401, B256::ZERO),
            SpecId::PRAGUE
        );
        // Jovian changes no EVM rules in the OP Stack config.
        assert_eq!(
            spec_id_for_header(10, 1, 1_764_691_201, B256::ZERO),
            SpecId::PRAGUE
        );
        assert_eq!(spec_id_for_block(10, 1), LATEST_LIVE_SPEC);
    }

    #[test]
    fn base_and_unknown_chains_use_the_latest_live_spec() {
        for chain in [8453, 999_999] {
            assert_eq!(
                spec_id_for_header(chain, 1, 0, B256::ZERO),
                LATEST_LIVE_SPEC
            );
            assert_eq!(spec_id_for_block(chain, 1), LATEST_LIVE_SPEC);
        }
    }
}
