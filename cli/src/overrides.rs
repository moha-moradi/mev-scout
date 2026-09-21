use crate::cli::{BlockRangeArgs, Command};
use mev_scout_core::config::CliOverrides;

fn apply_block_range(o: &mut CliOverrides, b: &BlockRangeArgs) {
    o.days = b.days;
    o.blocks = b.blocks;
    o.block = b.block;
    o.from_block = b.from_block;
    o.to_block = b.to_block;
}

pub fn build_overrides_from_command(cmd: &Command) -> CliOverrides {
    let mut o = CliOverrides::default();
    match cmd {
        Command::Run(args) => {
            apply_block_range(&mut o, &args.block_range);
        }
        Command::Report(_) => {}
        Command::Config => {}
        Command::Discover(args) => {
            apply_block_range(&mut o, &args.block_range);
        }
        Command::Tokens(_) => {}
        Command::Live(_) => {}
        Command::Explorer(_) => {}
        Command::Paper(args) => {
            use crate::cli::PaperCommand;
            if let PaperCommand::Run(a) = &args.command {
                apply_block_range(&mut o, &a.block_range);
            }
        }
    }
    o
}
