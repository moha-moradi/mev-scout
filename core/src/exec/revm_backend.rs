//! In-process revm backend. Injects the executor at [`super::EXECUTOR_ADDRESS`],
//! funds the searcher, and runs one flash-arb tx at a time via [`execute_whatif`].
use alloy::primitives::{keccak256, Address, Bytes, U256};
use revm::bytecode::Bytecode;
use revm::context::tx::TxEnv;
use revm::context_interface::transaction::AccessList;
use revm::database::CacheDB;
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::state::AccountInfo;

use crate::data::BlockData;
use crate::paper::types::{ExecAttempt, FillSkipReason};
use crate::replay::db::CachedRpcDb;
use crate::replay::{execute_whatif, BlockReplayer};

use super::artifacts::deployed_bytecode;
use super::calldata::encode_execute;
use super::plan::ExecPlan;
use super::EXECUTOR_ADDRESS;

/// Native wei given to the virtual searcher so the tx can pay gas.
const SEARCHER_FUND_WEI: u128 = 1_000 * 10u128.pow(18);
const GAS_LIMIT: u64 = 2_500_000;
const PRIORITY_WEI: u128 = 1_000_000_000;

pub struct RevmBlock {
    pub block_num: u64,
    pub db: Option<CacheDB<CachedRpcDb>>,
    pub block_data: BlockData,
    pub nonce: u64,
}

pub fn open_revm_block(
    replayer: &BlockReplayer,
    block_num: u64,
    searcher: Address,
) -> anyhow::Result<RevmBlock> {
    let mut db = replayer.what_if_state(block_num)?;
    let (block_data, _) = replayer.load_block_data(block_num)?;
    inject_executor(&mut db)?;
    fund_searcher(&mut db, searcher);
    Ok(RevmBlock {
        block_num,
        db: Some(db),
        block_data,
        nonce: 0,
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
    let cfg = replayer.build_cfg_env(block.block_num);
    let block_env = replayer.build_block_env(&block.block_data);
    let gas_price = (block_env.basefee as u128).saturating_add(PRIORITY_WEI);
    let nonce = block.nonce;
    let tx = TxEnv {
        tx_type: 2,
        caller: searcher,
        kind: TxKind::Call(EXECUTOR_ADDRESS),
        value: U256::ZERO,
        data: data.clone(),
        gas_limit: GAS_LIMIT,
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
    let (runs, db) = execute_whatif(db, cfg, block_env, &[tx])?;
    block.db = Some(db);
    let Some(run) = runs.into_iter().next() else {
        return Ok(ExecAttempt::failed(FillSkipReason::ExecBuildFailed));
    };
    if run.committed {
        block.nonce = block.nonce.saturating_add(1);
    }
    let net = run.delta_of(searcher);
    let gas_wei = (run.gas_used as u128).saturating_mul(gas_price);
    Ok(ExecAttempt::ran(
        run.status,
        net,
        run.gas_used,
        gas_wei,
        Some(synthetic_tx_hash(block.block_num, nonce, &data)),
    ))
}

fn inject_executor(db: &mut CacheDB<CachedRpcDb>) -> anyhow::Result<()> {
    let raw = deployed_bytecode()?;
    let code = Bytecode::new_raw(raw);
    let code_hash = code.hash_slow();
    db.insert_account_info(
        EXECUTOR_ADDRESS,
        AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code_hash,
            code: Some(code),
            account_id: None,
        },
    );
    Ok(())
}

fn fund_searcher(db: &mut CacheDB<CachedRpcDb>, searcher: Address) {
    db.insert_account_info(
        searcher,
        AccountInfo {
            balance: U256::from(SEARCHER_FUND_WEI),
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        },
    );
}

fn synthetic_tx_hash(block: u64, nonce: u64, data: &Bytes) -> String {
    let mut preimage = Vec::with_capacity(16 + data.len());
    preimage.extend_from_slice(&block.to_be_bytes());
    preimage.extend_from_slice(&nonce.to_be_bytes());
    preimage.extend_from_slice(data);
    format!("{:#x}", keccak256(preimage))
}
