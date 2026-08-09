use crate::cli::{BlockRangeArgs, ChainArgs, Cli, Command};
use mev_scout_core::config::CliOverrides;

fn apply_chain_args(o: &mut CliOverrides, c: &ChainArgs) {
    o.chain = Some(c.chain.clone());
    o.rpc.rpc_url = c.rpc_url.clone();
    o.rpc.rpc_urls = c.rpc_urls.clone();
    o.rpc.rpc_rps = c.rpc_rps.clone();
    o.rpc.ws_url = c.ws_url.clone();
    o.rpc.rps_limit = Some(c.rps_limit);
}

fn apply_block_range(o: &mut CliOverrides, b: &BlockRangeArgs) {
    o.days = b.days;
    o.blocks = b.blocks;
    o.block = b.block;
    o.from_block = b.from_block;
    o.to_block = b.to_block;
}

fn apply_storage_args(o: &mut CliOverrides, db_path: &Option<String>, parquet_dir: &Option<String>) {
    o.output.db_path.clone_from(db_path);
    o.output.parquet_dir.clone_from(parquet_dir);
}

fn apply_dune_chain_args(o: &mut CliOverrides, chain: &str, dune_api_key: &Option<String>) {
    o.chain = Some(chain.to_string());
    o.dune.dune_api_key.clone_from(dune_api_key);
}

pub fn build_overrides(cli: &Cli) -> CliOverrides {
    let mut o = CliOverrides::default();
    match &cli.command {
        Command::Run(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &args.parquet_dir);
            o.backtest.flash_loan_provider = Some(args.flash_loan_provider.clone());
            o.backtest.strategies = Some(args.strategies.clone());
            o.gas.gas_model = Some(args.gas_model.clone());
            o.gas.gas_limit = Some(args.gas_limit);
            o.gas.priority_fee_gwei = Some(args.priority_fee);
            o.output.output = Some(args.output.clone());
            o.output.export_path = Some(args.export_path.clone());
            o.backtest.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.backtest.token_prices = args.token_prices.clone();
            o.backtest.proximity_window = Some(args.proximity_window);
            o.backtest.capture_pending = Some(args.capture_pending);
        }
        Command::Fetch(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &args.parquet_dir);
            o.rpc.block_concurrency = args.block_concurrency;
        }
        Command::Replay(args) => {
            o.block = Some(args.block);
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &args.parquet_dir);
        }
        Command::Report(_) => {}
        Command::Config => {}
        Command::Discover(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &None);
        }
        Command::Live(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &None);
            o.backtest.strategies = Some(args.strategies.clone());
            o.gas.gas_model = Some(args.gas_model.clone());
            o.gas.gas_limit = Some(args.gas_limit);
            o.gas.priority_fee_gwei = Some(args.priority_fee);
            o.output.output = Some("json".to_string());
            o.output.export_path = Some(args.export_path.clone());
            o.backtest.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.backtest.token_prices = args.token_prices.clone();
            o.live.initial_balance = Some(args.initial_balance);
            o.live.min_profit_threshold = Some(args.min_profit);
            o.live.poll_interval_ms = Some(args.poll_interval);
            o.live.max_executions = args.max_executions;
        }
        Command::Audit(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            o.from_block = Some(args.from_block);
            o.to_block = Some(args.to_block);
        }
        Command::DuneCheck(args) => {
            apply_dune_chain_args(&mut o, &args.chain, &args.dune_api_key);
        }
        Command::DuneFindBlocks(args) => {
            apply_dune_chain_args(&mut o, &args.chain, &args.dune_api_key);
        }
        Command::DuneQuery(args) => {
            apply_dune_chain_args(&mut o, &args.chain, &args.dune_api_key);
        }
        Command::DuneReport(args) => {
            apply_dune_chain_args(&mut o, &args.chain, &args.dune_api_key);
        }
        Command::Tokens(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            o.dune.dune_api_key = args.dune_api_key.clone();
        }
    }
    o
}
