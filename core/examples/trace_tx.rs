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
use revm::interpreter::interpreter_types::Jumps;
use revm::interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, Interpreter, InterpreterTypes};
use revm::primitives::hardfork::SpecId;
use revm::primitives::TxKind;
use revm::context_interface::result::ExecutionResult;
use revm::{Context, InspectEvm};
use revm::inspector::Inspector;
use revm::inspector::inspectors::GasInspector;
use std::env;

struct StepTracer {
    gas_inspector: GasInspector,
    opcode: u8,
    ops: Vec<(u8, u64, u64)>,
}

impl<CTX, INTR: InterpreterTypes> Inspector<CTX, INTR> for StepTracer {
    fn initialize_interp(&mut self, interp: &mut Interpreter<INTR>, _ctx: &mut CTX) {
        self.gas_inspector.initialize_interp(&interp.gas);
    }

    fn step(&mut self, interp: &mut Interpreter<INTR>, _ctx: &mut CTX) {
        self.gas_inspector.step(&interp.gas);
        self.opcode = interp.bytecode.opcode();
    }

    fn step_end(&mut self, interp: &mut Interpreter<INTR>, _ctx: &mut CTX) {
        self.gas_inspector.step_end(&interp.gas);
        let cost = self.gas_inspector.last_gas_cost();
        let remaining = self.gas_inspector.gas_remaining();
        if cost > 0 {
            self.ops.push((self.opcode, cost, remaining));
        }
    }

    fn call_end(&mut self, _ctx: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        self.gas_inspector.call_end(outcome);
    }

    fn create_end(&mut self, _ctx: &mut CTX, _inputs: &CreateInputs, outcome: &mut CreateOutcome) {
        self.gas_inspector.create_end(outcome);
    }
}

fn op_name(op: u8) -> &'static str {
    match op {
        0x00 => "STOP", 0x01 => "ADD", 0x02 => "MUL", 0x03 => "SUB",
        0x04 => "DIV", 0x05 => "SDIV", 0x06 => "MOD", 0x07 => "SMOD",
        0x08 => "ADDMOD", 0x09 => "MULMOD", 0x0a => "EXP", 0x0b => "SIGNEXTEND",
        0x10 => "LT", 0x11 => "GT", 0x12 => "SLT", 0x13 => "SGT",
        0x14 => "EQ", 0x15 => "ISZERO", 0x16 => "AND", 0x17 => "OR",
        0x18 => "XOR", 0x19 => "NOT", 0x1a => "BYTE", 0x1b => "SHL",
        0x1c => "SHR", 0x1d => "SAR", 0x1e => "SHA3",
        0x30 => "ADDRESS", 0x31 => "BALANCE", 0x32 => "ORIGIN",
        0x33 => "CALLER", 0x34 => "CALLVALUE", 0x35 => "CALLDATALOAD",
        0x36 => "CALLDATASIZE", 0x37 => "CALLDATACOPY",
        0x38 => "CODESIZE", 0x39 => "CODECOPY", 0x3a => "GASPRICE",
        0x3b => "EXTCODESIZE", 0x3c => "EXTCODECOPY",
        0x3d => "RETURNDATASIZE", 0x3e => "RETURNDATACOPY",
        0x3f => "EXTCODEHASH", 0x40 => "BLOCKHASH",
        0x41 => "COINBASE", 0x42 => "TIMESTAMP", 0x43 => "NUMBER",
        0x44 => "DIFFICULTY", 0x45 => "GASLIMIT", 0x46 => "CHAINID",
        0x47 => "SELFBALANCE",
        0x48 => "BASEFEE", 0x49 => "BLOBHASH", 0x4a => "BLOBBASEFEE",
        0x50 => "POP", 0x51 => "MLOAD", 0x52 => "MSTORE", 0x53 => "MSTORE8",
        0x54 => "SLOAD", 0x55 => "SSTORE", 0x56 => "JUMP", 0x57 => "JUMPI",
        0x58 => "PC", 0x59 => "MSIZE", 0x5a => "GAS", 0x5b => "JUMPDEST",
        0x5c => "TLOAD", 0x5d => "TSTORE",
        0x5e => "MCOPY", 0x5f => "PUSH0",
        0x60..=0x7f => "PUSH", 0x80..=0x8f => "DUP",
        0x90..=0x9f => "SWAP", 0xa0..=0xa4 => "LOG",
        0xa5 => "PUSH128", 0xa6 => "PUSH256",
        0xf0 => "CREATE", 0xf1 => "CALL", 0xf2 => "CALLCODE",
        0xf3 => "RETURN", 0xf4 => "DELEGATECALL", 0xf5 => "CREATE2",
        0xfa => "STATICCALL", 0xfd => "REVERT", 0xff => "SELFDESTRUCT",
        _ => "???",
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

    let mut cache_db: CacheDB<CachedRpcDb> = if tx_index == 0 {
        CacheDB::new(CachedRpcDb::new(handle, cache, rpc, chain_id, block_num))
    } else {
        replayer.replay_to(block_num, tx_index - 1)?.0
    };
    mev_scout_core::replay::register_polygon_precompiles(&mut cache_db, block_num)?;

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

    let inspector = StepTracer {
        gas_inspector: GasInspector::new(),
        opcode: 0,
        ops: Vec::new(),
    };
    let ctx = Context::mainnet().with_db(cache_db).with_cfg(cfg).with_block(block_env);
    let mut evm = ctx.build_mainnet_with_inspector(inspector);
    let result = evm.inspect_one_tx(tx_env)?;

    let ops = std::mem::take(&mut evm.inspector.ops);
    println!("\n=== total gas-charging steps: {} ===", ops.len());

    let mut sload = 0u64;
    let mut sstore = 0u64;
    let mut call = 0u64;
    let mut log_gas = 0u64;
    let mut others = 0u64;
    let mut count_sload = 0u64;
    let mut count_sstore = 0u64;
    for (op, cost, _remaining) in &ops {
        if *op == 0x54 {
            count_sload += 1;
            sload += cost;
        } else if *op == 0x55 {
            count_sstore += 1;
            sstore += cost;
        } else if matches!(*op, 0xf1 | 0xf2 | 0xf3 | 0xf4 | 0xfa) {
            call += cost;
        } else if matches!(*op, 0xa0..=0xa4) {
            log_gas += cost;
        } else {
            others += cost;
        }
    }
    println!(
        "gas by class: SLOAD={sload} ({count_sload}x) SSTORE={sstore} ({count_sstore}x) CALLs={call} LOGs={log_gas} others={others}"
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
        "result: exec_gas_used={} status={:?}",
        result.tx_gas_used(),
        result.is_success()
    );
    println!(
        "receipt: gas_used={} delta={}",
        receipt.gas_used,
        receipt.gas_used.saturating_sub(result.tx_gas_used())
    );
    println!("gas: total_spent={spent} refunded={refunded} floor={floor}");

    // top-20 most expensive ops
    let mut sorted = ops.clone();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\n=== top-20 expensive ops ===");
    for (op, cost, remaining) in sorted.iter().take(20) {
        println!("  {} (0x{:02x}) cost={}", op_name(*op), op, cost);
    }

    // all SLOAD/SSTORE ops
    let ss_ops: Vec<_> = ops.iter().filter(|(op, _, _)| *op == 0x54 || *op == 0x55).collect();
    println!("\n=== SLOAD/SSTORE details ({} total) ===", ss_ops.len());
    for (op, cost, remaining) in ss_ops.iter().take(40) {
        println!("  {} cost={}", op_name(*op), cost);
    }

    Ok(())
}
