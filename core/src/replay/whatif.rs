//! What-if executor (MEV-VERIFICATION §B.1): run a candidate transaction bundle
//! against historical post-state and measure the executed native P&L — an
//! independent, revm-computed ground truth that never depends on a tracing RPC
//! result or the classifier's price attribution.
//!
//! `paper`'s modeled profit (`expected_profit − gas_cost_wei`) is a single pure
//! arithmetic claim; this module executes the *same tx* through revm so the two
//! can be reconciled. It also re-executes real on-chain transactions of a block
//! (`what_if_real_txs`) to extract the executed native delta per tx, derived
//! exactly like the explorer trace path but computed locally.
//!
//! ## Measurement semantics
//! Per-tx native delta = Σ over every account touched by the tx of
//! `post_balance − pre_balance`. Gas fees already flow through the caller's
//! balance change; the block `beneficiary` is excluded (it merely receives the
//! priority fee — the explorer trace path also never attributes validator
//! income to the searcher). Token notional profit is *not* measured here: this
//! gate answers "what native wei difference did executing this tx produce",
//! the same quantity `trace_native_delta_wei` reports.
//!
//! `transact` (journal-retaining) is deliberately used instead of the
//! `transact_commit` hot path: the un-committed `CacheDB` still holds the
//! pre-tx balances while the returned `EvmState` carries the post-tx ones, so
//! deltas are read before the caller's journal is dropped.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::result::EVMError;
use revm::database::CacheDB;
use revm::handler::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};
use revm::{Context, DatabaseRef};
use serde::Serialize;

use super::db::CachedRpcDb;
use super::replayer::BlockReplayer;

/// Executed outcome of one candidate transaction in a what-if bundle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WhatIfRun {
    /// Index of the tx within the candidate bundle / block.
    pub index: usize,
    /// `true` on success (no revert / halt / invalid-tx degradation).
    pub status: bool,
    /// revm gas used. For a degraded (invalid-tx) run this is the full
    /// `gas_limit` the caller would have burned.
    pub gas_used: u64,
    /// Per-address native balance deltas (post − pre) for accounts touched by
    /// this tx, coinbase excluded. Zero deltas are dropped.
    pub deltas: Vec<(Address, i128)>,
}

impl WhatIfRun {
    /// Sum of native balance deltas across all touched accounts. Gas-inclusive
    /// (the caller's balance already drops by fees). Negative ⇒ the tx cost
    /// native; positive ⇒ it captured native value.
    pub fn net_wei(&self) -> i128 {
        self.deltas.iter().map(|(_, d)| *d).sum()
    }
}

/// Executed net for one opportunity anchor (canonical id / tx hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecutedNet {
    /// Native wei delta (gas-inclusive) — the what-if executed result.
    pub net_wei: i128,
    pub gas_used: u64,
    /// `false` when the executed tx reverted / halted / degraded.
    pub status: bool,
}

impl ExecutedNet {
    pub fn from_run(run: &WhatIfRun) -> ExecutedNet {
        ExecutedNet {
            net_wei: run.net_wei(),
            gas_used: run.gas_used,
            status: run.status,
        }
    }
}

/// Executed results keyed by an opportunity anchor (canonical id in the paper
/// reconciliation; tx hash elsewhere).
pub type ExecutedNetMap = HashMap<String, ExecutedNet>;

/// Reduce parallel `runs` + `keys` into an [`ExecutedNetMap`].
pub fn executed_map(runs: &[WhatIfRun], keys: &[String]) -> ExecutedNetMap {
    runs.iter()
        .zip(keys)
        .map(|(r, k)| (k.clone(), ExecutedNet::from_run(r)))
        .collect()
}

/// Execute a candidate bundle against a caller-supplied database as one EVM
/// block, returning one [`WhatIfRun`] per tx. Txs commit sequentially, so the
/// N-th runs against the post-state of txs `0..N` (bundle semantics).
pub fn execute_whatif(
    db: CacheDB<CachedRpcDb>,
    cfg: CfgEnv,
    block: BlockEnv,
    txs: &[TxEnv],
) -> anyhow::Result<Vec<WhatIfRun>> {
    let beneficiary = block.beneficiary;
    let mut evm = Context::mainnet()
        .with_db(db)
        .with_cfg(cfg)
        .with_block(block)
        .build_mainnet();
    let mut runs = Vec::with_capacity(txs.len());
    for (i, tx) in txs.iter().enumerate() {
        let exec_and_state = match evm.transact(tx.clone()) {
            Ok(e) => e,
            Err(err) => {
                if matches!(err, EVMError::Database(_)) {
                    return Err(anyhow::anyhow!(
                        "what-if state load failed for tx {i}: {err}"
                    ));
                }
                tracing::warn!("what-if tx {i} execution error: {err:?}");
                runs.push(WhatIfRun {
                    index: i,
                    status: false,
                    gas_used: tx.gas_limit,
                    deltas: Vec::new(),
                });
                continue;
            }
        };
        let status = exec_and_state.result.is_success();
        let gas_used = exec_and_state.result.tx_gas_used();
        let mut deltas: Vec<(Address, i128)> = Vec::new();
        for (addr, acc) in &exec_and_state.state {
            if *addr == beneficiary {
                continue;
            }
            let pre = balance_of(&evm.ctx.journaled_state.database, *addr)?;
            let post = acc.info.balance;
            let d = delta_i128(pre, post);
            if d != 0 {
                deltas.push((*addr, d));
            }
        }
        evm.commit(exec_and_state.state);
        runs.push(WhatIfRun {
            index: i,
            status,
            gas_used,
            deltas,
        });
    }
    Ok(runs)
}

/// Pre-tx balance of `addr` from revm's un-committed database.
fn balance_of(db: &CacheDB<CachedRpcDb>, addr: Address) -> anyhow::Result<U256> {
    db.basic_ref(addr)
        .map(|a| a.map(|info| info.balance).unwrap_or(U256::ZERO))
        .map_err(|e| anyhow::anyhow!("what-if balance read for {addr:#x}: {e}"))
}

/// Post − pre balance delta, saturated to `i128`/`i128::MIN/MAX`.
fn delta_i128(pre: U256, post: U256) -> i128 {
    if post >= pre {
        let d = post - pre;
        if d > U256::from(i128::MAX as u128) {
            i128::MAX
        } else {
            d.to::<i128>()
        }
    } else {
        let d = pre - post;
        if d > U256::from(i128::MAX as u128) {
            i128::MIN
        } else {
            -(d.to::<i128>())
        }
    }
}

impl BlockReplayer {
    /// Pre-block state snapshot for `block_num` — the database execution
    /// against which the block's own txs (and any what-if bundle) run.
    pub fn what_if_state(&self, block_num: u64) -> anyhow::Result<CacheDB<CachedRpcDb>> {
        self.create_db_for_block(block_num)
    }

    /// Execute a candidate bundle against `db` as an append of `block_num`.
    pub fn what_if(
        &self,
        block_num: u64,
        db: CacheDB<CachedRpcDb>,
        txs: &[TxEnv],
    ) -> anyhow::Result<Vec<WhatIfRun>> {
        let (block, _txs) = self.load_block_data(block_num)?;
        execute_whatif(
            db,
            self.build_cfg_env(block_num),
            self.build_block_env(&block),
            txs,
        )
    }

    /// Execute a hypothetical bundle against the pre-block state.
    pub fn what_if_from_block_start(
        &self,
        block_num: u64,
        txs: &[TxEnv],
    ) -> anyhow::Result<Vec<WhatIfRun>> {
        let db = self.what_if_state(block_num)?;
        self.what_if(block_num, db, txs)
    }

    /// Re-execute the real on-chain txs of `block_num` in one sequential pass
    /// and capture an executed [`WhatIfRun`] at each requested `indices`
    /// entry. Deterministic given a warm cache + state RPC (the same state the
    /// backtest already replays against); the deltas give the executed native
    /// P&L of each anchored tx for paper reconciliation (§B.3).
    pub fn what_if_real_txs(
        &self,
        block_num: u64,
        indices: &[usize],
    ) -> anyhow::Result<Vec<(usize, WhatIfRun)>> {
        let (block, txs) = self.load_block_data(block_num)?;
        let mut want: std::collections::BTreeSet<usize> = indices.iter().copied().collect();
        let beneficiary = block.coinbase;
        let mut evm = Context::mainnet()
            .with_db(self.create_db_for_block(block_num)?)
            .with_cfg(self.build_cfg_env(block_num))
            .with_block(self.build_block_env(&block))
            .build_mainnet();
        let mut out = Vec::new();
        for (i, tx) in txs.iter().enumerate() {
            let tx_env = self.tx_data_to_tx_env(tx);
            if !want.remove(&i) {
                match evm.transact_commit(tx_env) {
                    Ok(_) => {}
                    Err(err) => {
                        if matches!(err, EVMError::Database(_)) {
                            return Err(anyhow::anyhow!(
                                "what-if state load failed for block {block_num} tx {i}: {err}"
                            ));
                        }
                        tracing::warn!(
                            "what-if block {block_num} tx {i} skipped with error: {err:?}"
                        );
                    }
                }
                continue;
            }
            let exec_and_state = match evm.transact(tx_env) {
                Ok(e) => e,
                Err(err) => {
                    if matches!(err, EVMError::Database(_)) {
                        return Err(anyhow::anyhow!(
                            "what-if state load failed for block {block_num} tx {i}: {err}"
                        ));
                    }
                    tracing::warn!("what-if block {block_num} tx {i} execution error: {err:?}");
                    out.push((
                        i,
                        WhatIfRun {
                            index: i,
                            status: false,
                            gas_used: tx.gas_limit,
                            deltas: Vec::new(),
                        },
                    ));
                    continue;
                }
            };
            let status = exec_and_state.result.is_success();
            let gas_used = exec_and_state.result.tx_gas_used();
            let mut deltas: Vec<(Address, i128)> = Vec::new();
            for (addr, acc) in &exec_and_state.state {
                if *addr == beneficiary {
                    continue;
                }
                let pre = balance_of(&evm.ctx.journaled_state.database, *addr)?;
                let post = acc.info.balance;
                let d = delta_i128(pre, post);
                if d != 0 {
                    deltas.push((*addr, d));
                }
            }
            evm.commit(exec_and_state.state);
            out.push((
                i,
                WhatIfRun {
                    index: i,
                    status,
                    gas_used,
                    deltas,
                },
            ));
        }
        Ok(out)
    }

    /// Convenience for a single anchored real tx: see [`BlockReplayer::what_if_real_txs`].
    pub fn what_if_real_tx(
        &self,
        block_num: u64,
        tx_index: usize,
    ) -> anyhow::Result<Option<WhatIfRun>> {
        Ok(self
            .what_if_real_txs(block_num, &[tx_index])?
            .into_iter()
            .next()
            .map(|(_, run)| run))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Bytes, B256};
    use revm::context_interface::block::BlobExcessGasAndPrice;
    use revm::context_interface::transaction::AccessList;
    use revm::primitives::hardfork::SpecId;
    use revm::primitives::{TxKind, KECCAK_EMPTY};
    use revm::state::AccountInfo;

    use crate::cache::SqliteStore;
    use crate::rpc::RpcClient;

    fn address_0x1() -> Address {
        address!("1111111111111111111111111111111111111111")
    }
    fn address_0x2() -> Address {
        address!("2222222222222222222222222222222222222222")
    }
    fn address_0x3() -> Address {
        address!("3333333333333333333333333333333333333333")
    }
    const COINBASE: Address = address!("9999999999999999999999999999999999999999");
    const BASE_FEE: u64 = 1_000_000_000; // 1 gwei
    const ONE_ETH: u128 = 1_000_000_000_000_000_000;
    // effective price (base 1 gwei + priority 1 gwei) × intrinsic gas 21k.
    const TX_FEE_WEI: i128 = 21_000i128 * 2_000_000_000i128;

    fn tx(caller: Address, to: Address, value: U256, nonce: u64, gas_limit: u64) -> TxEnv {
        // EIP-1559: max fee 2 gwei, priority 1 gwei over a 1 gwei base fee.
        TxEnv {
            tx_type: 2,
            caller,
            kind: TxKind::Call(to),
            value,
            data: Bytes::new(),
            gas_limit,
            gas_price: 2_000_000_000,
            gas_priority_fee: Some(1_000_000_000),
            nonce,
            access_list: AccessList(Vec::new()),
            chain_id: Some(1),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            authorization_list: Vec::new(),
        }
    }

    /// Warm `CacheDB` whose accounts are pre-inserted into the in-memory cache,
    /// so no account lookup ever reaches the (dead) RPC.
    fn warm_db() -> CacheDB<CachedRpcDb> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let rt = RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        });
        let dir = std::env::temp_dir().join(format!(
            "whatif_unit_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let store = SqliteStore::open(dir.join("cache.sqlite")).unwrap();
        let handle = rt.handle().clone();
        let rpc = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
        let mut db = CacheDB::new(CachedRpcDb::new(handle, store, rpc, 1, 0));
        for (addr, balance, nonce) in [
            (address_0x1(), U256::from(10 * ONE_ETH), 0u64),
            (address_0x2(), U256::from(3 * ONE_ETH), 0u64),
            (address_0x3(), U256::ZERO, 0u64),
            // The EVM credits the priority fee to the block beneficiary, so it
            // must be warm too or the first cache miss would hit the dead RPC.
            (COINBASE, U256::ZERO, 0u64),
        ] {
            db.insert_account_info(
                addr,
                AccountInfo {
                    nonce,
                    balance,
                    code: None,
                    code_hash: KECCAK_EMPTY,
                    account_id: None,
                },
            );
        }
        db
    }

    fn cfg_env() -> CfgEnv {
        let mut cfg = CfgEnv::new_with_spec(SpecId::CANCUN);
        cfg.chain_id = 1;
        cfg.limit_contract_code_size = Some(0x6000);
        cfg
    }

    fn block_env() -> BlockEnv {
        BlockEnv {
            number: U256::from(1),
            beneficiary: COINBASE,
            timestamp: U256::from(12345678),
            gas_limit: 30_000_000,
            basefee: BASE_FEE,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::repeat_byte(0x11)),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new_with_spec(
                0,
                SpecId::CANCUN,
            )),
            slot_num: 0,
        }
    }

    #[test]
    fn value_transfer_measures_caller_and_recipient_deltas() {
        let runs = execute_whatif(
            warm_db(),
            cfg_env(),
            block_env(),
            &[tx(
                address_0x1(),
                address_0x2(),
                U256::from(ONE_ETH),
                0,
                50_000,
            )],
        )
        .unwrap();
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert!(run.status, "plain transfer must succeed");
        assert_eq!(run.gas_used, 21_000, "intrinsic gas for a value transfer");
        assert_eq!(run.net_wei(), -TX_FEE_WEI, "gas-inclusive native net");
        // Deltas: sender pays value + fee; recipient +value; coinbase excluded.
        assert_eq!(run.deltas.len(), 2);
        let a = run
            .deltas
            .iter()
            .find(|(a, _)| *a == address_0x1())
            .map(|(_, d)| *d)
            .unwrap();
        let b = run
            .deltas
            .iter()
            .find(|(a, _)| *a == address_0x2())
            .map(|(_, d)| *d)
            .unwrap();
        assert_eq!(a, -(ONE_ETH as i128) - TX_FEE_WEI);
        assert_eq!(b, ONE_ETH as i128);
        assert!(
            !run.deltas.iter().any(|(a, _)| *a == COINBASE),
            "coinbase (validator tip) must not be attributed as searcher net"
        );
    }

    #[test]
    fn sequential_bundle_commits_state_and_nonces() {
        let runs = execute_whatif(
            warm_db(),
            cfg_env(),
            block_env(),
            &[
                tx(address_0x1(), address_0x2(), U256::from(ONE_ETH), 0, 50_000),
                tx(address_0x1(), address_0x3(), U256::from(ONE_ETH), 1, 50_000),
            ],
        )
        .unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].status && runs[1].status);
        // Tx1 runs against post-tx0 state (sender nonce advanced); both paid the fee.
        assert_eq!(runs[1].net_wei(), -TX_FEE_WEI);
        let a2 = runs[1]
            .deltas
            .iter()
            .find(|(a, _)| *a == address_0x1())
            .map(|(_, d)| *d)
            .unwrap();
        assert_eq!(a2, -(ONE_ETH as i128) - TX_FEE_WEI);
    }

    #[test]
    fn insufficient_gas_degrades_to_status_false() {
        let runs = execute_whatif(
            warm_db(),
            cfg_env(),
            block_env(),
            &[tx(
                address_0x1(),
                address_0x2(),
                U256::from(ONE_ETH),
                0,
                15_000,
            )],
        )
        .unwrap();
        let run = &runs[0];
        assert!(!run.status, "gas below intrinsic must not succeed");
        assert_eq!(
            run.gas_used, 15_000,
            "full gas_limit charged on degradation"
        );
        assert!(run.deltas.is_empty());
        assert_eq!(run.net_wei(), 0);
    }

    #[test]
    fn executed_map_keys_runs_by_anchor() {
        let runs = execute_whatif(
            warm_db(),
            cfg_env(),
            block_env(),
            &[tx(
                address_0x1(),
                address_0x2(),
                U256::from(ONE_ETH),
                0,
                50_000,
            )],
        )
        .unwrap();
        let keys = vec!["TwoHopArb|0x1".to_string()];
        let map = executed_map(&runs, &keys);
        let e = &map["TwoHopArb|0x1"];
        assert!(e.status);
        assert_eq!(e.gas_used, 21_000);
        assert!(e.net_wei < 0);
    }
}
