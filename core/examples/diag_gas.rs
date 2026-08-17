use alloy::primitives::U256;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::replay::{BlockReplayer, CachedRpcDb};
use mev_scout_core::rpc::RpcClient;
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::cfg::GasParams;
use revm::context_interface::transaction::AccessList;
use revm::database::CacheDB;
use revm::handler::{MainBuilder, MainContext};
use revm::primitives::hardfork::SpecId;
use revm::primitives::TxKind;
use revm::context_interface::result::ExecutionResult;
use revm::{Context, ExecuteCommitEvm};
use std::env;

fn main() -> anyhow::Result<()> {
    let toml = env::args().nth(1).unwrap_or_else(|| "mev-scout.toml".into());
    let block_num: u64 = env::args().nth(2).unwrap_or_else(|| "92053774".into()).parse()?;
    let start: usize = env::args().nth(3).unwrap_or_else(|| "0".into()).parse()?;
    let end: usize = env::args().nth(4).unwrap_or_else(|| "30".into()).parse()?;

    let config = Config::load(&toml)?;
    let chain_name: mev_scout_core::types::ChainName = config.chain.parse()?;
    let chain_id = chain_name.chain_id();
    let provider_configs = config.effective_provider_configs(chain_name)?;
    let urls: Vec<&str> = provider_configs.iter().map(|(u, _, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&urls, chain_id)?;

    let rt = tokio::runtime::Runtime::new()?;
    let handle = rt.handle().clone();
    rt.block_on(async {
        rpc.with_provider_rps(
            &provider_configs
                .iter()
                .map(|(_, r, _)| r.unwrap_or(config.rpc.rps_limit))
                .collect::<Vec<_>>(),
        )
        .await;
        rpc.with_provider_archive(&provider_configs.iter().map(|(_, _, a)| *a).collect::<Vec<_>>())
            .await;
    });

    let db_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(db_path)?;

    let replayer = BlockReplayer::new(handle.clone(), cache.clone(), rpc.clone(), chain_id);
    let (block, txs) = replayer.load_block_data(block_num)?;
    let receipts = replayer.load_receipts(block_num)?;

    let spec = mev_scout_core::replay::spec_id_for_block(chain_id, block_num);
    let gas_params = GasParams::new_spec(spec);

    println!("block {block_num} spec={spec:?} {} txs", txs.len());

    let mut cache_db: CacheDB<CachedRpcDb> = if start == 0 {
        CacheDB::new(CachedRpcDb::new(
            handle.clone(), cache.clone(), rpc.clone(), chain_id, block_num.saturating_sub(1),
        ))
    } else {
        replayer.replay_to(block_num, start - 1)?.0
    };
    mev_scout_core::replay::register_polygon_precompiles(&mut cache_db, block_num)?;

    let mut cfg = CfgEnv::new_with_spec(spec);
    cfg.chain_id = chain_id;
    cfg.limit_contract_code_size = Some(0x6000);

    let blob_excess_gas_and_price = if spec >= SpecId::CANCUN {
        Some(BlobExcessGasAndPrice::new_with_spec(0, spec))
    } else {
        None
    };
    let block_env = BlockEnv {
        number: U256::from(block.number),
        beneficiary: block.coinbase,
        timestamp: U256::from(block.timestamp),
        gas_limit: block.gas_limit,
        basefee: block.base_fee_per_gas.unwrap_or(0) as u64,
        difficulty: U256::ZERO,
        prevrandao: Some(alloy::primitives::B256::ZERO),
        blob_excess_gas_and_price,
        slot_num: 0,
    };

    let ctx = Context::mainnet()
        .with_db(cache_db)
        .with_cfg(cfg)
        .with_block(block_env);
    let mut evm = ctx.build_mainnet();

    println!("idx | type | revm_gas | receipt | delta | total_spent | refunded | floor(revm) | floor(manual) | intrinsic(revm) | intrinsic(manual) | calldata | auth | al_addrs | al_keys | to");
    println!("----|------|----------|---------|-------|-------------|----------|-------------|---------------|-----------------|-------------------|----------|-----|---------|---------|----");

    for i in start..end.min(txs.len()) {
        let tx = &txs[i];
        let receipt = &receipts[i];

        let calldata_len = tx.input.len();
        let al_addrs = tx.access_list.len() as u64;
        let al_keys: u64 = tx.access_list.iter().map(|a| a.slots.len() as u64).sum();
        let auth_count = tx.authorization_list.len() as u64;

        let zero_bytes = tx.input.iter().filter(|b| **b == 0).count() as u64;
        let nonzero_bytes = calldata_len as u64 - zero_bytes;
        let floor_tokens_manual = zero_bytes + nonzero_bytes * 4;
        let floor_manual = 10 * floor_tokens_manual + 21000
            + (al_addrs + al_keys * 4) * 10;
        let intrinsic_manual = 21000 + (zero_bytes * 4) + (nonzero_bytes * 16)
            + (al_addrs * 2400) + (al_keys * 1900);

        let kind = match tx.to {
            Some(addr) => TxKind::Call(addr),
            None => TxKind::Create,
        };

        let tx_env = TxEnv {
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
                tx.access_list.iter().map(|item| revm::context_interface::transaction::AccessListItem {
                    address: item.address,
                    storage_keys: item.slots.to_vec(),
                }).collect(),
            ),
            chain_id: Some(chain_id),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            authorization_list: tx.authorization_list.iter().map(|a| {
                alloy::signers::Either::Left(alloy::eips::eip7702::SignedAuthorization::new_unchecked(
                    alloy::eips::eip7702::Authorization {
                        chain_id: a.chain_id,
                        address: a.address,
                        nonce: a.nonce,
                    },
                    a.y_parity,
                    a.r,
                    a.s,
                ))
            }).collect(),
        };

        let init_gas = gas_params.initial_tx_gas_for_tx(&tx_env);
        let floor_revm = init_gas.floor_gas();
        let intrinsic_revm = init_gas.initial_regular_gas();
        let state_gas_revm = init_gas.initial_state_gas;

        match evm.transact_commit(tx_env) {
            Ok(result) => {
                let (total_spent, refunded, floor_result) = match &result {
                    ExecutionResult::Success { gas, .. }
                    | ExecutionResult::Revert { gas, .. }
                    | ExecutionResult::Halt { gas, .. } => (
                        gas.total_gas_spent(),
                        gas.inner_refunded(),
                        gas.floor_gas(),
                    ),
                };
                let revm_gas = result.tx_gas_used();
                let delta = receipt.gas_used as i64 - revm_gas as i64;
                let floor_applied = if revm_gas as u64 == floor_result { "Y" } else { "N" };
                println!(
                    "{i:3} | {:#04x} | {revm_gas:8} | {:7} | {delta:+6} | {total_spent:11} | {refunded:8} | {floor_revm:11} | {floor_manual:13} | {intrinsic_revm:15} | {intrinsic_manual:17} | {:8} | {:3} | {:7} | {:7} | {} floor={floor_applied} state={state_gas_revm}",
                    tx.tx_type,
                    receipt.gas_used,
                    calldata_len,
                    auth_count,
                    al_addrs,
                    al_keys,
                    tx.to.map(|a| a.to_string()).unwrap_or_else(|| "create".into()),
                );
            }
            Err(e) => {
                println!("{i:3} | ERROR: {e:?}");
            }
        }
    }

    Ok(())
}
