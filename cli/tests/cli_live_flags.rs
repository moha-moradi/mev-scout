use clap::Parser;
use mev_scout_cli::cli::{Cli, Command};

/// WP4/M-08: all four capital-free live flags are part of the CLI surface,
/// not library-only overrides.
#[test]
fn live_capital_free_flags_parse() {
    let cli = Cli::try_parse_from([
        "mev-scout",
        "live",
        "--funding-provider",
        "aave",
        "--max-quote-age-ms",
        "750",
        "--require-risk-clear",
        "--pending-backrun",
        "--optimizer-sweep",
    ])
    .expect("flags must parse");
    let Command::Live(args) = cli.command_or_default() else {
        panic!("expected live subcommand");
    };
    assert_eq!(args.funding_provider.as_deref(), Some("aave"));
    assert_eq!(args.max_quote_age_ms, Some(750));
    assert!(args.require_risk_clear);
    assert!(args.pending_backrun);
    assert!(args.optimizer_sweep);
}

#[test]
fn live_flags_default_to_disabled() {
    let args = match Cli::try_parse_from(["mev-scout", "live"])
        .expect("bare live must parse")
        .command_or_default()
    {
        Command::Live(a) => a,
        _ => panic!("expected live subcommand"),
    };
    assert_eq!(args.funding_provider, None);
    assert_eq!(args.max_quote_age_ms, None);
    assert!(!args.require_risk_clear);
    assert!(!args.pending_backrun);
    assert!(!args.optimizer_sweep);
}
