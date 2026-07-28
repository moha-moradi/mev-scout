use crate::cli::{BlockRangeArgs, ChainArgs, Cli, Command};
use mev_scout_core::config::CliOverrides;

fn apply_chain_args(o: &mut CliOverrides, c: &ChainArgs) {
    o.chain = Some(c.chain.clone());
    o.rpc_url = c.rpc_url.clone();
    o.rpc_urls = c.rpc_urls.clone();
    o.rpc_rps = c.rpc_rps.clone();
    o.rps_limit = Some(c.rps_limit);
}

fn apply_block_range(o: &mut CliOverrides, b: &BlockRangeArgs) {
    o.days = b.days;
    o.blocks = b.blocks;
    o.block = b.block;
    o.from_block = b.from_block;
    o.to_block = b.to_block;
}

pub fn build_overrides(cli: &Cli) -> CliOverrides {
    let mut o = CliOverrides::default();
    match &cli.command {
        Command::Run(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            o.flash_loan_provider = Some(args.flash_loan_provider.clone());
            o.strategies = Some(args.strategies.clone());
            o.gas_model = Some(args.gas_model.clone());
            o.gas_limit = Some(args.gas_limit);
            o.priority_fee_gwei = Some(args.priority_fee);
            o.output = Some(args.output.clone());
            o.export_path = Some(args.export_path.clone());
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
            o.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.token_prices = args.token_prices.clone();
            o.proximity_window = Some(args.proximity_window);
            o.capture_pending = Some(args.capture_pending);
            o.cross_block_window = Some(args.cross_block_window);
        }
        Command::Fetch(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            o.block_concurrency = args.block_concurrency;
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
        }
        Command::Replay(args) => {
            o.block = Some(args.block);
            apply_chain_args(&mut o, &args.chain_args);
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
        }
        Command::Report(_) => {}
        Command::Config => {}
        Command::Discover(args) => {
            apply_block_range(&mut o, &args.block_range);
            apply_chain_args(&mut o, &args.chain_args);
            o.db_path = args.db_path.clone();
        }
        Command::Live(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            o.strategies = Some(args.strategies.clone());
            o.gas_model = Some(args.gas_model.clone());
            o.gas_limit = Some(args.gas_limit);
            o.priority_fee_gwei = Some(args.priority_fee);
            o.output = Some("json".to_string());
            o.export_path = Some(args.export_path.clone());
            o.db_path = args.db_path.clone();
            o.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.token_prices = args.token_prices.clone();
            o.initial_balance = Some(args.initial_balance);
            o.min_profit_threshold = Some(args.min_profit);
            o.poll_interval_ms = Some(args.poll_interval);
            o.max_executions = args.max_executions;
        }
        Command::Audit(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            o.from_block = Some(args.from_block);
            o.to_block = Some(args.to_block);
        }
        Command::DuneCheck(args) => {
            o.chain = Some(args.chain.clone());
            o.dune_api_key = args.dune_api_key.clone();
        }
        Command::DuneFindBlocks(args) => {
            o.chain = Some(args.chain.clone());
            o.dune_api_key = args.dune_api_key.clone();
        }
        Command::DuneQuery(args) => {
            o.chain = Some(args.chain.clone());
            o.dune_api_key = args.dune_api_key.clone();
        }
        Command::Tokens(args) => {
            apply_chain_args(&mut o, &args.chain_args);
            o.dune_api_key = args.dune_api_key.clone();
        }
    }
    o
}
