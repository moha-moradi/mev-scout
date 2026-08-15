use alloy::primitives::U256;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::replay::{BlockReplayer, CachedRpcDb};
use mev_scout_core::rpc::RpcClient;
use revm::context::block::BlockEnv;
use revm::context::cfg::CfgEnv;
use revm::context::tx::TxEnv;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::transaction::AccessList;
use revm::database::CacheDB;
use revm::handler::{MainBuilder, MainContext};
use revm::interpreter::{Interpreter, InterpreterTypes};
use revm::interpreter::interpreter_types::Jumps;
use revm::primitives::hardfork::SpecId;
use revm::primitives::TxKind;
use revm::context_interface::result::ExecutionResult;
use revm::{Context, InspectEvm, Inspector};
use std::env;

struct StepTracer {
    prev: Option<u64>,
    ops: Vec<(String, u64, u64)>,
}

impl<CTX, INTR: InterpreterTypes> Inspector<CTX, INTR> for StepTracer {
    fn step(&mut self, interp: &mut Interpreter<INTR>, _ctx: &mut CTX) {
        let now = interp.gas.remaining();
        let op = interp.bytecode.opcode().to_string();
        if let Some(p) = self.prev {
            let cost = p.saturating_sub(now);
            self.ops.push((op, cost, now));
        }
        self.prev = Some(now);
    }
}

fn main() -> anyhow::Result<()> {
    let toml = env::args().nth(1).unwrap_or_else(|| "mev-scout.toml".into());
    let block_num: u64 = env::args().nth(2).unwrap_or_else(|| "92053774".into()).parse()?;
    let tx_index: usize = env::args().nth(3).unwrap_or_else(|| "24".into()).parse()?;

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

    let tx = &txs[tx_index];
    let receipt = &receipts[tx_index];
    println!(
        "tx {tx_index}: to={} gas_limit={} value={} calldata={}",
        tx.to.map(|a| a.to_string()).unwrap_or_else(|| "create".into()),
        tx.gas_limit,
        tx.value,
        tx.input.len()
    );

    // Replay from tx 0 up to tx_index-1 to build correct intermediate state.
    let (mut cache_db, _results) = if tx_index == 0 {
        let cache_db = CacheDB::new(CachedRpcDb::new(
            handle,
            cache,
            rpc,
            chain_id,
            block_num,
        ));
        mev_scout_core::replay::register_polygon_precompiles(&mut cache_db, block_num)?;
        (cache_db, Vec::new())
    } else {
        replayer.replay_to(block_num, tx_index - 1)?
    };

    // Build the EVM with an inspector and run tx_index.
    let spec = mev_scout_core::replay::spec_id_for_block(chain_id, block_num);
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
        access_list: AccessList(vec![]),
        chain_id: Some(chain_id),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    };

    let inspector = StepTracer { prev: None, ops: Vec::new() };
    let ctx = Context::mainnet()
        .with_db(cache_db)
        .with_cfg(cfg)
        .with_block(block_env);
    let mut evm = ctx.build_mainnet_with_inspector(inspector);
    let result = evm.inspect_one_tx(tx_env)?;

    let ops = std::mem::take(&mut evm.inspector.ops);
    println!("\n=== total steps: {} ===", ops.len());

    let mut sload = 0u64;
    let mut sstore = 0u64;
    let mut call = 0u64;
    let mut logs = 0u64;
    let mut others = 0u64;
    let mut count_sload = 0u64;
    let mut count_sstore = 0u64;
    for (op, cost, _after) in &ops {
        if op == "SLOAD" {
            count_sload += 1;
            sload += cost;
        } else if op == "SSTORE" {
            count_sstore += 1;
            sstore += cost;
        } else if ["CALL", "STATICCALL", "DELEGATECALL", "CALLCODE"].contains(&op.as_str()) {
            call += cost;
        } else if op.starts_with("LOG") {
            logs += cost;
        } else {
            others += cost;
        }
    }
    println!(
        "gas by op class: SLOAD={sload} ({count_sload} ops) SSTORE={sstore} ({count_sstore} ops) CALLs={call} LOGs={logs} others={others}"
    );

    let (refunded, floor, spent) = match &result {
        ExecutionResult::Success { gas, .. }
        | ExecutionResult::Revert { gas, .. }
        | ExecutionResult::Halt { gas, .. } => (
            gas.inner_refunded(),
            gas.floor_gas(),
            gas.total_gas_spent(),
        ),
    };
    println!(
        "result: gas_used={} status={:?}",
        result.tx_gas_used(),
        result.is_success()
    );
    println!(
        "receipt: gas_used={} delta={}",
        receipt.gas_used,
        receipt.gas_used.saturating_sub(result.tx_gas_used())
    );
    println!("gas: total_spent={spent} refunded={refunded} floor={floor}");

    // Print top-20 most expensive ops
    let mut sorted = ops.clone();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\n=== top-20 expensive ops ===");
    for (op, cost, _after) in sorted.iter().take(20) {
        println!("  {op} cost={cost}");
    }

    // Print all SLOAD/SSTORE details with costs
    println!("\n=== SLOAD/SSTORE details ===");
    for (op, cost, _after) in &ops {
        if op == "SLOAD" || op == "SSTORE" {
            println!("  {op} cost={cost}");
        }
    }

    Ok(())
}
