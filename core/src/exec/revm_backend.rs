//! In-process revm backend. Deploys the executor via real CREATE, funds the
//! searcher from the ledger wallet, and runs flash-arb txs via [`execute_whatif`].
use alloy::primitives::{keccak256, Address, Bytes, U256};
use revm::context::block::BlockEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::transaction::AccessList;
use revm::database::CacheDB;
use revm::primitives::hardfork::SpecId;
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::state::AccountInfo;
use revm::DatabaseRef;

use crate::data::BlockData;
use crate::paper::types::{ExecAttempt, FillSkipReason};
use crate::replay::db::CachedRpcDb;
use crate::replay::{execute_whatif, BlockReplayer};
use crate::types::{DetectionPath, MevOpportunity};

use super::artifacts::{append_address_args, creation_bytecode};
use super::calldata::encode_execute;
use super::plan::ExecPlan;

const DEPLOY_GAS: u64 = 3_000_000;
/// Amsterdam+ (EIP-8037) moves code deposit to state gas at 1530/byte, so a
/// 6.5KB executor no longer deploys within the pre-Amsterdam limit.
const DEPLOY_GAS_STATE_GAS: u64 = 15_000_000;
const EXEC_GAS: u64 = 2_500_000;
const PRIORITY_WEI: u128 = 1_000_000_000;

fn deploy_gas_for(spec: SpecId) -> u64 {
    if spec.is_enabled_in(SpecId::AMSTERDAM) {
        DEPLOY_GAS_STATE_GAS
    } else {
        DEPLOY_GAS
    }
}

pub struct RevmBlock {
    pub state_block: u64,
    pub exec_env: BlockEnv,
    pub db: Option<CacheDB<CachedRpcDb>>,
    pub executor: Address,
    pub nonce: u64,
    /// Gas spent deploying the executor (setup overhead; not booked as a fill).
    pub setup_gas_wei: u128,
}

/// Open a revm block: position state, fund searcher, CREATE-deploy the executor.
pub fn open_revm_block(
    replayer: &BlockReplayer,
    chain_id: u64,
    opp: &MevOpportunity,
    searcher: Address,
    withdrawer: Address,
    weth: Address,
    balance_wei: u128,
) -> anyhow::Result<RevmBlock> {
    let pending = opp.mempool_only || matches!(opp.detection_path, Some(DetectionPath::Pending));
    let n = opp.block_number;

    let (mut db, exec_env, state_block) = if pending {
        // `opp.block_number` is the pending (not-yet-mined) block; the parent
        // is the newest cached block and seeds the next-block environment.
        let parent = n.saturating_sub(1);
        let (block_data, _) = replayer.load_block_data(parent)?;
        let db = replayer.what_if_state(n)?;
        let env = replayer.build_next_block_env(&block_data);
        (db, env, n)
    } else {
        match replayer.load_block_data(n) {
            Ok((block_data, txs)) if !txs.is_empty() => {
                let mut db = replayer.what_if_state(n)?;
                let cfg = replayer.build_cfg_env(n);
                let env = replayer.build_block_env(&block_data);
                let end = (opp.tx_index + 1).min(txs.len());
                let envs: Vec<TxEnv> = txs[..end]
                    .iter()
                    .map(|tx| replayer.tx_data_to_tx_env(tx))
                    .collect();
                if !envs.is_empty() {
                    let (_, next) = execute_whatif(db, cfg, env.clone(), &envs)?;
                    db = next;
                }
                (db, env, n)
            }
            Ok((block_data, _)) => {
                tracing::warn!(
                    "exec revm block {n}: no txs for positioning; using pre-block state"
                );
                let db = replayer.what_if_state(n)?;
                let env = replayer.build_block_env(&block_data);
                (db, env, n)
            }
            Err(e) => {
                tracing::warn!(
                    "exec revm block {n}: block data unavailable ({e}); pre-block state only"
                );
                let db = replayer.what_if_state(n)?;
                // Minimal env from what we can load; fall back via what_if_state path.
                let (block_data, _) = replayer.load_block_data(n).unwrap_or_else(|_| {
                    (
                        BlockData {
                            number: n,
                            hash: Default::default(),
                            timestamp: opp.timestamp,
                            base_fee_per_gas: Some(1_000_000_000),
                            gas_limit: 30_000_000,
                            gas_used: 0,
                            coinbase: Address::ZERO,
                            difficulty: U256::ZERO,
                            mix_hash: Default::default(),
                        },
                        Vec::new(),
                    )
                });
                let env = replayer.build_block_env(&block_data);
                (db, env, n)
            }
        }
    };

    fund_searcher(&mut db, searcher, balance_wei)?;

    let deploy_nonce = db
        .basic_ref(searcher)
        .ok()
        .flatten()
        .map(|info| info.nonce)
        .unwrap_or(0);
    let expected = searcher.create(deploy_nonce);

    let mut creation = creation_bytecode()?;
    creation = append_address_args(&creation, &[searcher, withdrawer, weth]);
    let gas_price = (exec_env.basefee as u128).saturating_add(PRIORITY_WEI);
    let deploy_gas =
        deploy_gas_for(crate::replay::spec_id_for_block(chain_id, n)).min(exec_env.gas_limit);
    let deploy_tx = TxEnv {
        tx_type: 2,
        caller: searcher,
        kind: TxKind::Create,
        value: U256::ZERO,
        data: creation,
        gas_limit: deploy_gas,
        gas_price,
        gas_priority_fee: Some(PRIORITY_WEI),
        nonce: deploy_nonce,
        access_list: AccessList(Vec::new()),
        chain_id: Some(chain_id),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    };
    let cfg = replayer.build_cfg_env(n);
    let (runs, db) = execute_whatif(db, cfg, exec_env.clone(), &[deploy_tx])?;
    let run = runs
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("revm deploy produced no run"))?;
    if !run.status || !run.committed {
        anyhow::bail!(
            "revm executor CREATE failed (status={}, committed={}, gas_used={}, created={:?})",
            run.status,
            run.committed,
            run.gas_used,
            run.created
        );
    }
    let created = run
        .created
        .ok_or_else(|| anyhow::anyhow!("revm CREATE missing address"))?;
    if created != expected {
        anyhow::bail!("revm CREATE address mismatch: got {created:#x}, expected {expected:#x}");
    }
    let setup_gas_wei = (run.gas_used as u128).saturating_mul(gas_price);
    let next_nonce = deploy_nonce.saturating_add(1);

    Ok(RevmBlock {
        state_block,
        exec_env,
        db: Some(db),
        executor: created,
        nonce: next_nonce,
        setup_gas_wei,
    })
}

pub fn exec_plan_revm(
    replayer: &BlockReplayer,
    chain_id: u64,
    block: &mut RevmBlock,
    searcher: Address,
    plan: &ExecPlan,
) -> anyhow::Result<ExecAttempt> {
    let data = encode_execute(plan);
    let gas_price = (block.exec_env.basefee as u128).saturating_add(PRIORITY_WEI);
    let db = block
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("revm db already consumed"))?;
    let nonce = db
        .basic_ref(searcher)
        .ok()
        .flatten()
        .map(|info| info.nonce)
        .unwrap_or(block.nonce);
    let tx = TxEnv {
        tx_type: 2,
        caller: searcher,
        kind: TxKind::Call(block.executor),
        value: U256::ZERO,
        data: data.clone(),
        gas_limit: EXEC_GAS,
        gas_price,
        gas_priority_fee: Some(PRIORITY_WEI),
        nonce,
        access_list: AccessList(Vec::new()),
        chain_id: Some(chain_id),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    };
    let db = block
        .db
        .take()
        .ok_or_else(|| anyhow::anyhow!("revm db already consumed"))?;
    let cfg = replayer.build_cfg_env(block.state_block);
    let (runs, db) = execute_whatif(db, cfg, block.exec_env.clone(), &[tx])?;
    block.db = Some(db);
    let Some(run) = runs.into_iter().next() else {
        return Ok(ExecAttempt::failed(FillSkipReason::ExecBuildFailed));
    };
    if run.committed {
        block.nonce = nonce.saturating_add(1);
    }
    let net = run.delta_of(searcher);
    let gas_wei = (run.gas_used as u128).saturating_mul(gas_price);
    Ok(ExecAttempt::ran(
        run.status,
        net,
        run.gas_used,
        gas_wei,
        Some(synthetic_tx_hash(block.state_block, nonce, &data)),
    ))
}

fn fund_searcher(
    db: &mut CacheDB<CachedRpcDb>,
    searcher: Address,
    balance_wei: u128,
) -> anyhow::Result<()> {
    let existing = db.basic_ref(searcher).ok().flatten();
    let (nonce, code_hash, code) = match existing {
        Some(info) => (info.nonce, info.code_hash, info.code),
        None => (0, KECCAK_EMPTY, None),
    };
    db.insert_account_info(
        searcher,
        AccountInfo {
            balance: U256::from(balance_wei),
            nonce,
            code_hash,
            code,
            account_id: None,
        },
    );
    Ok(())
}

fn synthetic_tx_hash(block: u64, nonce: u64, data: &Bytes) -> String {
    let mut preimage = Vec::with_capacity(16 + data.len());
    preimage.extend_from_slice(&block.to_be_bytes());
    preimage.extend_from_slice(&nonce.to_be_bytes());
    preimage.extend_from_slice(data);
    format!("{:#x}", keccak256(preimage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256};
    use revm::context::cfg::CfgEnv;
    use revm::context_interface::block::BlobExcessGasAndPrice;
    use revm::database::EmptyDB;
    use revm::primitives::hardfork::SpecId;

    use crate::cache::SqliteStore;
    use crate::data::{AccountData, BlockData};
    use crate::rpc::RpcClient;
    use crate::types::{DetectionPath, MevOpportunity, Strategy};

    #[test]
    fn create_address_matches_searcher_nonce() {
        let searcher = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            searcher,
            AccountInfo {
                balance: U256::from(10u128.pow(18)),
                nonce: 0,
                code_hash: KECCAK_EMPTY,
                code: None,
                account_id: None,
            },
        );
        let tx = TxEnv {
            tx_type: 2,
            caller: searcher,
            kind: TxKind::Create,
            value: U256::ZERO,
            // Minimal initcode: PUSH1 0 PUSH1 0 RETURN → empty runtime code.
            data: Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]),
            gas_limit: 100_000,
            gas_price: 1_000_000_000,
            gas_priority_fee: Some(1),
            nonce: 0,
            access_list: AccessList(Vec::new()),
            chain_id: Some(1),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            authorization_list: Vec::new(),
        };
        let mut cfg = CfgEnv::new_with_spec(SpecId::CANCUN);
        cfg.chain_id = 1;
        let block = BlockEnv {
            number: U256::from(1u64),
            beneficiary: Address::ZERO,
            timestamp: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::repeat_byte(0x11)),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new_with_spec(
                0,
                SpecId::CANCUN,
            )),
            slot_num: 0,
        };
        let expected = searcher.create(0);
        let (runs, db) = execute_whatif(db, cfg, block, &[tx]).expect("whatif");
        assert_eq!(runs.len(), 1);
        assert!(
            runs[0].status,
            "create should succeed: committed={} created={:?}",
            runs[0].committed, runs[0].created
        );
        assert_eq!(runs[0].created, Some(expected));
        let after = db.basic_ref(searcher).ok().flatten().expect("searcher");
        assert_eq!(after.nonce, 1);
    }

    /// Pending opportunities are stamped with the pending block number (the
    /// next block). Positioning must use the cached parent block and open the
    /// state after that parent, not look up the not-yet-mined block.
    #[test]
    fn pending_block_positions_on_parent_state() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let searcher = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let weth = address!("0x00000000000000000000000000000000000000aa");
        // Polygon Cancun block: avoids revm's experimental NEXT (Amsterdam)
        // EIP-8037 state-gas costs, which are irrelevant to positioning.
        let parent = 60_000_000u64;
        let pending = parent + 1;

        let dir = std::env::temp_dir().join(format!(
            "revm_pending_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = SqliteStore::open(dir.join("cache.sqlite")).expect("cache");
        let parent_block = BlockData {
            number: parent,
            hash: B256::ZERO,
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(1_000_000_000),
            gas_limit: 30_000_000,
            gas_used: 0,
            coinbase: searcher,
            difficulty: U256::ZERO,
            mix_hash: B256::ZERO,
        };
        store.put_block(parent, &parent_block).expect("block");
        store.put_txs(parent, &[]).expect("txs");

        // Keep the deploy offline: pre-seed the searcher and the CREATE
        // address it will produce at the parent's post-state.
        let created = searcher.create(0);
        for addr in [searcher, created] {
            store
                .put_account(
                    parent,
                    addr,
                    &AccountData {
                        nonce: 0,
                        balance: U256::ZERO,
                        code_hash: KECCAK_EMPTY,
                    },
                )
                .expect("account");
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        // Polygon state setup probes precompiles; fail those calls fast so the
        // test never reaches the network.
        let server = rt.block_on(async {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::any())
                .respond_with(wiremock::ResponseTemplate::new(500))
                .mount(&server)
                .await;
            server
        });
        let rpc = RpcClient::new(&server.uri(), 1).expect("rpc");
        let replayer = BlockReplayer::new(rt.handle().clone(), store, rpc, 137);

        let mut opp = MevOpportunity::new(pending, 0, Strategy::TwoHopArb, Address::ZERO, 0);
        opp.mempool_only = true;
        opp.detection_path = Some(DetectionPath::Pending);
        opp.timestamp = parent_block.timestamp + 12;

        let opened = open_revm_block(
            &replayer,
            137,
            &opp,
            searcher,
            searcher,
            weth,
            10u128.pow(18),
        )
        .expect("pending block should open from the cached parent");

        assert_eq!(opened.state_block, pending);
        assert_eq!(opened.exec_env.number, U256::from(pending));
        assert_eq!(
            opened.exec_env.timestamp,
            U256::from(parent_block.timestamp + 12)
        );
        assert_eq!(opened.exec_env.basefee, 1_125_000_000);
        assert_eq!(opened.executor, created);
        assert_eq!(opened.nonce, 1);
        assert!(opened.setup_gas_wei > 0);
    }

    /// A recent chain-1 block must map to its real fork (Osaka), not revm's
    /// experimental `NEXT`; the deploy uses a type-2 tx and must land.
    #[test]
    fn chain_one_recent_block_deploys_under_real_spec() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let searcher = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let weth = address!("0x00000000000000000000000000000000000000aa");
        let block_num = 24_000_000u64;
        let state_block = block_num - 1;

        let dir = std::env::temp_dir().join(format!(
            "revm_chain1_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = SqliteStore::open(dir.join("cache.sqlite")).expect("cache");
        let block = BlockData {
            number: block_num,
            hash: B256::ZERO,
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(1_000_000_000),
            gas_limit: 30_000_000,
            gas_used: 0,
            coinbase: searcher,
            difficulty: U256::ZERO,
            mix_hash: B256::ZERO,
        };
        store.put_block(block_num, &block).expect("block");
        store.put_txs(block_num, &[]).expect("txs");

        let created = searcher.create(0);
        for addr in [searcher, created] {
            store
                .put_account(
                    state_block,
                    addr,
                    &AccountData {
                        nonce: 0,
                        balance: U256::ZERO,
                        code_hash: KECCAK_EMPTY,
                    },
                )
                .expect("account");
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let rpc = RpcClient::new("http://0.0.0.0:1", 1).expect("rpc");
        let replayer = BlockReplayer::new(rt.handle().clone(), store, rpc, 1);

        let opp = MevOpportunity::new(block_num, 0, Strategy::TwoHopArb, Address::ZERO, 0);
        let opened = open_revm_block(&replayer, 1, &opp, searcher, searcher, weth, 10u128.pow(18))
            .expect("chain-1 deploy should cover state-gas deposit costs");

        assert_eq!(opened.state_block, block_num);
        assert_eq!(opened.executor, created);
        assert_eq!(opened.nonce, 1);
        assert!(opened.setup_gas_wei > 0);
    }

    #[test]
    fn deploy_gas_keeps_three_million_before_state_gas_spec() {
        assert_eq!(deploy_gas_for(SpecId::CANCUN), DEPLOY_GAS);
        assert_eq!(deploy_gas_for(SpecId::PRAGUE), DEPLOY_GAS);
        assert_eq!(deploy_gas_for(SpecId::AMSTERDAM), DEPLOY_GAS_STATE_GAS);
    }
}
