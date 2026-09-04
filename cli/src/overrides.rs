use crate::cli::{BlockRangeArgs, Cli, Command};
use mev_scout_core::config::CliOverrides;

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
        }
        Command::Fetch(args) => {
            apply_block_range(&mut o, &args.block_range);
        }
        Command::Replay(args) => {
            o.block = Some(args.block);
        }
        Command::Report(_) => {}
        Command::Config => {}
        Command::Discover(args) => {
            apply_block_range(&mut o, &args.block_range);
        }
        Command::Tokens(_) => {}
        Command::ValidatePools(_) => {}
        Command::Scan(args) => {
            apply_block_range(&mut o, &args.block_range);
        }
        Command::Live(_) => {}
    }
    o
}