use crate::cli::{BlockRangeArgs, ChainArgs, Cli, Command};
use mev_scout_core::config::CliOverrides;

fn apply_chain_args(o: &mut CliOverrides, c: &ChainArgs) {
    o.chain = Some(c.chain.clone());
    o.rpc.rpc_url = c.rpc_url.clone();
    o.rpc.rpc_urls = c.rpc_urls.clone();
    o.rpc.rpc_rps = c.rpc_rps.clone();
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
        Command::Tokens(args) => {
            apply_chain_args(&mut o, &args.chain_args);
        }
        Command::ValidatePools(args) => {
            apply_chain_args(&mut o, &args.chain_args);
        }
        Command::Scan(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
        }
        Command::Live(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            apply_storage_args(&mut o, &args.db_path, &None);
            o.backtest.flash_loan_provider = Some(args.flash_loan_provider.clone());
            o.backtest.strategies = Some(args.strategies.clone());
            o.gas.gas_model = Some(args.gas_model.clone());
            o.gas.gas_limit = Some(args.gas_limit);
            o.gas.priority_fee_gwei = Some(args.priority_fee);
            o.output.output = Some(args.output.clone());
            o.output.export_path = Some(args.export_path.clone());
            o.backtest.proximity_window = Some(args.proximity_window);
            o.backtest.min_profit_wei = Some(args.min_profit_wei);
        }
    }
    o
}
