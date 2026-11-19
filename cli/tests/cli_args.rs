#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{
    example_config_str, expect_fail, expect_ok, make_cfg, run_timed, scout, temp_ws, TimedOutput,
    TEST_TIMEOUT,
};
use std::path::Path;
use std::time::Duration;

fn cfg() -> String {
    example_config_str()
}

fn run(ws: &Path, args: &[&str]) -> TimedOutput {
    let mut c = scout(ws);
    c.args(args);
    run_timed(&mut c, TEST_TIMEOUT).expect("spawn/wait failed")
}

fn help_lists_subcommand(stdout: &str, name: &str) -> bool {
    stdout.lines().any(|l| {
        let t = l.trim_start();
        t == name || t.starts_with(&format!("{name} "))
    })
}

#[test]
fn help_lists_kept_commands() {
    let ws = temp_ws("args_help");
    let out = run(&ws, &["--help"]);
    expect_ok(&out, "mev-scout --help");
    for cmd in ["report", "config", "discover", "live", "explorer"] {
        assert!(
            help_lists_subcommand(&out.stdout, cmd),
            "--help output missing subcommand '{cmd}'"
        );
    }
    for removed in [
        "tokens",
        "run",
        "paper",
        "fetch",
        "replay",
        "validate-pools",
        "scan",
    ] {
        assert!(
            !help_lists_subcommand(&out.stdout, removed),
            "--help still lists removed subcommand '{removed}'"
        );
    }
}

#[test]
fn explorer_help_lists_subcommands() {
    let ws = temp_ws("args_explorer_help");
    let out = run(&ws, &["explorer", "--help"]);
    expect_ok(&out, "explorer --help");
    assert!(
        help_lists_subcommand(&out.stdout, "index"),
        "explorer --help missing subcommand 'index'"
    );
    for removed in [
        "dashboard",
        "purge-retired",
        "show",
        "report",
        "backfill",
        "stats",
        "doctor",
        "live-feed",
        "top",
        "explain",
        "export",
        "validate",
    ] {
        assert!(
            !help_lists_subcommand(&out.stdout, removed),
            "explorer --help still lists removed subcommand '{removed}'"
        );
    }
    for removed in [
        "--dashboard",
        "--db",
        "--format",
        "--repeat",
        "--kind",
        "--confidence",
    ] {
        assert!(
            !out.stdout.contains(removed),
            "explorer --help still lists removed flag '{removed}'"
        );
    }
    assert!(
        out.stdout.contains("--out"),
        "explorer --help missing retained flag '--out'"
    );
    assert!(
        !out.stdout.contains("--window"),
        "range flags belong on the index subcommand, not bare explorer"
    );
    for removed in [
        "--windows",
        "--top",
        "--with-gaps",
        "--max-ops",
        "--evidence",
        "--budget-mb",
    ] {
        assert!(
            !out.stdout.contains(removed),
            "explorer --help still lists removed flag '{removed}'"
        );
    }
}

#[test]
fn live_help_lists_ledger_flags() {
    let ws = temp_ws("args_live_ledger_help");
    let out = run(&ws, &["live", "--help"]);
    expect_ok(&out, "live --help");
    for flag in ["--initial-balance", "--reserve", "--exec", "--sim"] {
        assert!(
            out.stdout.contains(flag),
            "live --help missing ledger flag '{flag}'"
        );
    }
    assert!(
        !out.stdout.to_ascii_lowercase().contains("testnet"),
        "live --help must not mention testnet"
    );
    // Price-injection and internal tuning knobs moved to config-only.
    for removed in [
        "--initial-balance-usd",
        "--max-fills-per-block",
        "--native-usd",
    ] {
        assert!(
            !out.stdout.contains(removed),
            "live --help still lists removed flag '{removed}'"
        );
    }
}

#[test]
fn live_exec_testnet_is_rejected() {
    let ws = temp_ws("args_live_exec_testnet");
    let cfg_path = make_cfg(&ws, &[]);
    let out = run(
        &ws,
        &[
            "-f", &cfg_path, "live", "--exec", "testnet", "--blocks", "1",
        ],
    );
    expect_fail(&out, "live --exec testnet");
    let combined = out.combined().to_ascii_lowercase();
    assert!(
        combined.contains("expected") || combined.contains("invalid"),
        "expected rejection of testnet, got:\n{}",
        out.combined()
    );
}

/// Removed / unknown CLI surface — one table-driven gate instead of N clones.
#[test]
fn removed_and_unknown_surface_fails() {
    let cases: &[(&str, &[&str], Option<&str>)] = &[
        (
            "tokens",
            &["tokens"],
            Some("unrecognized subcommand 'tokens'"),
        ),
        ("explorer stats", &["explorer", "stats"], None),
        ("explorer report", &["explorer", "report"], None),
        ("explorer backfill", &["explorer", "backfill"], None),
        ("explorer dashboard", &["explorer", "dashboard"], None),
        (
            "explorer purge-retired",
            &["explorer", "purge-retired"],
            None,
        ),
        ("explorer top", &["explorer", "top", "--by", "sender"], None),
        ("explorer unknown", &["explorer", "frobnicate"], None),
        ("explorer --windows", &["explorer", "--windows", "7d"], None),
        ("explorer --top", &["explorer", "--top", "20"], None),
        (
            "explorer --with-gaps",
            &["explorer", "--dashboard", "--with-gaps"],
            None,
        ),
        (
            "explorer --max-ops",
            &["explorer", "--dashboard", "--max-ops", "10"],
            None,
        ),
        (
            "explorer --evidence",
            &["explorer", "--dashboard", "--evidence", "10"],
            None,
        ),
        (
            "explorer --budget-mb",
            &["explorer", "--dashboard", "--budget-mb", "1"],
            None,
        ),
        (
            "explorer index --hours",
            &["explorer", "index", "--hours", "6"],
            None,
        ),
        (
            "explorer index --days",
            &["explorer", "index", "--days", "1"],
            None,
        ),
        ("paper", &["paper"], Some("unrecognized subcommand 'paper'")),
        ("run", &["run"], Some("unrecognized subcommand 'run'")),
        ("fetch", &["fetch"], None),
        ("scan", &["scan"], None),
        ("replay", &["replay"], None),
        ("unknown root", &["frobnicate"], None),
    ];
    for (label, args, needle) in cases {
        let ws = temp_ws(&format!("args_removed_{}", label.replace(' ', "_")));
        let out = run(&ws, args);
        expect_fail(&out, &format!("removed/unknown surface '{label}'"));
        if let Some(n) = needle {
            assert!(
                out.combined().contains(n),
                "expected clap message containing {n:?} for '{label}', got:\n{}",
                out.combined()
            );
        }
    }
}

#[test]
fn config_prints_resolved_toml_from_example_toml() {
    let ws = temp_ws("args_config");
    let out = run(&ws, &["-f", &cfg(), "config"]);
    expect_ok(&out, "config with mev-scout.example.toml");
    assert!(
        out.stdout.contains("polygon"),
        "config output should mention chain polygon"
    );
    // Dynamic provider count: compare against the example TOML itself instead of
    // a hard-coded threshold, so adding/removing providers doesn't break the
    // test. The resolved config must include at least every placeholder URL.
    // Commented examples (the env-interpolation form) are documentation only.
    let example_https_count = common::example_config_text()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| line.contains("https://"))
        .count();
    assert!(
        example_https_count >= 1,
        "mev-scout.example.toml should contain at least one provider placeholder"
    );
    let https_count = out.stdout.matches("https://").count();
    assert!(
        https_count >= example_https_count,
        "config should resolve all {example_https_count} example providers, found {https_count} https URLs"
    );
}

#[test]
fn report_error_paths_fail_cleanly() {
    let ws = temp_ws("args_report_err");

    // report on a fresh workspace with no recorded runs
    let out = run(&ws, &["-f", &cfg(), "report"]);
    expect_fail(&out, "report with no run history");
    assert!(
        out.stderr.contains("no runs recorded"),
        "expected 'no runs recorded' error, got: {}",
        out.stderr
    );

    // report with a non-existent --run-id
    let out = run(
        &ws,
        &["-f", &cfg(), "report", "--run-id", "nonexistent_run_id"],
    );
    expect_fail(&out, "report with non-existent run-id");
    assert!(
        out.stderr.contains("not found"),
        "expected run-not-found error, got: {}",
        out.stderr
    );
}

#[test]
fn invalid_duration_format_rejected_before_network() {
    let ws = temp_ws("args_bad_duration");
    let started = std::time::Instant::now();
    let out = run(
        &ws,
        &["-f", &cfg(), "live", "--loop", "--duration", "notatime"],
    );
    expect_fail(&out, "live --duration notatime");
    assert!(started.elapsed() < Duration::from_secs(60));
}

// â”€â”€ Block-range validation (offline; clap + validation.rs paths) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
//
// These live on `discover`, the remaining CLI surface that flattens
// `BlockRangeArgs`. The range is optional: with no flag, core falls back to the
// chain's `pool_discovery_lookback_blocks`. Only the clap-level rejections are
// asserted here, because `discover` calls `init_rpc` before it resolves a range
// â€” the runtime range branches (paired --from-block/--to-block, range order,
// mutually-exclusive flags) are therefore covered as unit tests against
// `RangeSpec::from_flags` in core/src/config/validation.rs, where they stay
// offline.

#[test]
fn blocks_zero_rejected_by_clap() {
    let ws = temp_ws("args_blocks_0");
    let out = run(&ws, &["discover", "--blocks", "0"]);
    expect_fail(&out, "discover --blocks 0");
    assert!(
        out.stderr.contains("error") || out.stdout.contains("error"),
        "expected clap error output"
    );
}

#[test]
fn block_zero_rejected_by_clap() {
    let ws = temp_ws("args_block_0");
    let out = run(&ws, &["discover", "--block", "0"]);
    expect_fail(&out, "discover --block 0");
    assert!(
        out.stderr.contains("error") || out.stdout.contains("error"),
        "expected clap error output"
    );
}

// â”€â”€ Config file loading behavior (missing file â†’ defaults; parse error â†’ fail) â”€â”€

#[test]
fn missing_config_file_falls_back_to_default() {
    let ws = temp_ws("args_cfg_missing");
    let out = run(&ws, &["-f", "definitely_missing_file.toml", "config"]);
    expect_ok(
        &out,
        "config with missing -f file (documented fallback to default)",
    );
    assert!(
        out.stdout.contains("chain = \"polygon\""),
        "fallback default config should resolve the default chain, got:\n{}",
        out.stdout
    );
}

#[test]
fn broken_toml_config_file_is_rejected() {
    let ws = temp_ws("args_cfg_broken");
    let broken = ws.join("broken.toml");
    std::fs::write(&broken, "not valid toml [[[").unwrap();
    let out = run(&ws, &["-f", broken.to_str().unwrap(), "config"]);
    expect_fail(
        &out,
        "config with broken TOML should fail (not silently fall back to default)",
    );
    assert!(
        out.stderr.contains("parse") || out.stderr.contains("error"),
        "stderr should explain the parse failure, got:\n{}",
        out.stderr
    );
}

#[test]
fn config_env_var_placeholders_expand_from_environment() {
    let ws = temp_ws("args_cfg_env_expand");
    let cfg_path = make_cfg(
        &ws,
        &[(
            "rpc_urls",
            "[\"https://rpc.example/v2/${MS_E2E_TEST_KEY}\"]",
        )],
    );

    // Without the variable set the placeholder stays verbatim (visible in the
    // resolved config) â€” a missing key never silently corrupts the URL.
    let out = run(&ws, &["-f", &cfg_path, "config"]);
    expect_ok(&out, "config without env var set");
    assert!(
        out.stdout.contains("${MS_E2E_TEST_KEY}"),
        "unset placeholder must stay verbatim in resolved config, got:\n{}",
        out.stdout
    );

    // With the variable exported the URL is expanded at load time.
    std::env::set_var("MS_E2E_TEST_KEY", "live-key-42");
    let out = run(&ws, &["-f", &cfg_path, "config"]);
    expect_ok(&out, "config with env var exported");
    assert!(
        out.stdout.contains("https://rpc.example/v2/live-key-42"),
        "placeholder must expand from the environment, got:\n{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("${MS_E2E_TEST_KEY}"),
        "expanded placeholder must not remain in resolved config, got:\n{}",
        out.stdout
    );
    std::env::remove_var("MS_E2E_TEST_KEY");
}

// â”€â”€ Global flags & misc offline behaviors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn version_flag_exits_zero_with_version() {
    let ws = temp_ws("args_version");
    let out = run(&ws, &["--version"]);
    expect_ok(&out, "mev-scout --version");
    assert!(
        out.stdout.split_whitespace().count() >= 2
            && out.stdout.chars().any(|c| c.is_ascii_digit()),
        "--version should print '<name> <version>', got: {}",
        out.stdout
    );
}

/// A bare `mev-scout` is the zero-config entrypoint: it must parse without a
/// usage error and dispatch to the implicit `live` run.
///
/// The config parses but fails `validate_live` (negative `winning_bid_premium`),
/// so the run stops at validation — offline and fast. Reaching that error at all
/// proves the default subcommand was materialized and dispatched; a clap usage
/// error would mean it was not.
#[test]
fn no_subcommand_parses_and_defaults_to_live() {
    let ws = temp_ws("args_no_cmd");
    let bad_gas = make_cfg(&ws, &[("winning_bid_premium", "-1.0")]);

    let started = std::time::Instant::now();
    let out = run(&ws, &["-f", &bad_gas]);
    assert!(
        !(out.stderr.contains("Usage") || out.stderr.contains("usage")),
        "bare invocation must not produce a clap usage error, got:\n{}",
        out.stderr
    );
    // `live` wraps `validate_live` with this context, and nothing else does.
    assert!(
        out.combined().contains("invalid configuration"),
        "expected the implicit `live` run to reach its config validation, got:\n{}",
        out.combined()
    );
    // Must fail fast: nothing may touch the network before validation.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "bare invocation must fail fast without touching the network"
    );
}

/// The typed default must match the clap defaults for `live`, so bare
/// `mev-scout` and explicit `mev-scout live` are equivalent.
#[test]
fn default_subcommand_matches_live_defaults() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command};

    let bare = Cli::parse_from(["mev-scout"]).command_or_default();
    let explicit = Cli::parse_from(["mev-scout", "live"]).command_or_default();
    match (bare, explicit) {
        (Command::Live(a), Command::Live(b)) => {
            assert_eq!(a.blocks, b.blocks, "--blocks default must agree");
            assert!(!a.r#loop, "bare invocation must be single-pass");
            assert!(a.duration.is_none());
            assert!(a.max_blocks.is_none());
        }
        _ => panic!("both invocations must resolve to `live`"),
    }
}

/// Bare `explorer` emits the all-time HTML dashboard; `--out` is its only flag.
#[test]
fn bare_explorer_emits_dashboard_by_default() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command};

    let cli = Cli::parse_from(["mev-scout", "explorer"]);
    match cli.command {
        Some(Command::Explorer(a)) => {
            assert!(a.command.is_none(), "bare explorer has no subcommand");
            assert!(a.out.is_none(), "output path is auto-derived by default");
        }
        other => panic!("expected Explorer, got {other:?}"),
    }

    let cli = Cli::parse_from(["mev-scout", "explorer", "--out", "custom.html"]);
    match cli.command {
        Some(Command::Explorer(a)) => {
            assert_eq!(a.out, Some(std::path::PathBuf::from("custom.html")));
        }
        other => panic!("expected Explorer, got {other:?}"),
    }
}

/// `explorer index` is a bounded historical backfill (default 7 days) that
/// exits through the HTML dashboard.
#[test]
fn explorer_index_defaults_to_backfill_shape() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command, ExplorerCommand};

    let cli = Cli::parse_from(["mev-scout", "explorer", "index"]);
    match cli.command {
        Some(Command::Explorer(a)) => match a.command {
            Some(ExplorerCommand::Index(i)) => {
                assert!(i.window.is_none());
                assert!(i.from_block.is_none());
                assert!(i.to_block.is_none());
                assert!(a.out.is_none());
            }
            other => panic!("expected Index, got {other:?}"),
        },
        other => panic!("expected Explorer, got {other:?}"),
    }

    for window in ["6h", "30d", "2w"] {
        let cli = Cli::parse_from(["mev-scout", "explorer", "index", "--window", window]);
        match cli.command {
            Some(Command::Explorer(a)) => match a.command {
                Some(ExplorerCommand::Index(i)) => {
                    assert_eq!(i.window.as_deref(), Some(window));
                    assert!(i.from_block.is_none());
                    assert!(i.to_block.is_none());
                }
                other => panic!("expected Index, got {other:?}"),
            },
            other => panic!("expected Explorer, got {other:?}"),
        }
    }

    let cli = Cli::parse_from([
        "mev-scout",
        "explorer",
        "index",
        "--from-block",
        "65000000",
        "--to-block",
        "65001000",
    ]);
    match cli.command {
        Some(Command::Explorer(a)) => match a.command {
            Some(ExplorerCommand::Index(i)) => {
                assert_eq!(i.from_block, Some(65_000_000));
                assert_eq!(i.to_block, Some(65_001_000));
                assert!(i.window.is_none());
            }
            other => panic!("expected Index, got {other:?}"),
        },
        other => panic!("expected Explorer, got {other:?}"),
    }
}

/// `--out` is global within the explorer group: both positions parse.
#[test]
fn explorer_out_flag_accepts_both_positions() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command, ExplorerCommand};

    for args in [
        [
            "mev-scout",
            "explorer",
            "index",
            "--window",
            "24h",
            "--out",
            "x.html",
        ],
        [
            "mev-scout",
            "explorer",
            "--out",
            "x.html",
            "index",
            "--window",
            "24h",
        ],
    ] {
        let cli = Cli::parse_from(args);
        match cli.command {
            Some(Command::Explorer(a)) => {
                assert_eq!(a.out, Some(std::path::PathBuf::from("x.html")));
                assert!(matches!(a.command, Some(ExplorerCommand::Index(_))));
            }
            other => panic!("expected Explorer, got {other:?}"),
        }
    }
}

/// Removed explorer flags are unknown arguments on both bare explorer and index.
#[test]
fn explorer_removed_flags_are_rejected() {
    for flag in [
        &["--db", "snapshot.sqlite"][..],
        &["--format", "json"][..],
        &["--repeat", "60s"][..],
        &["--confidence", "exact"][..],
        &["--kind", "sandwich"][..],
        &["--dashboard"][..],
    ] {
        for prefix in [
            &["explorer"][..],
            &["explorer", "index", "--window", "1d"][..],
        ] {
            let mut args: Vec<&str> = prefix.to_vec();
            args.extend_from_slice(flag);
            let ws = temp_ws(&format!(
                "args_explorer_removed_{}",
                args.join("_").replace(['-', '/', '.'], "_")
            ));
            let out = run(&ws, &args);
            expect_fail(&out, &format!("removed explorer flag {args:?}"));
            let combined = out.combined();
            assert!(
                combined.contains("unexpected argument")
                    || combined.contains("unrecognized")
                    || combined.contains("unknown"),
                "expected clap unknown-arg rejection for {args:?}, got:\n{combined}"
            );
        }
    }
}

/// Invalid index windows fail before any RPC initialization.
#[test]
fn explorer_index_rejects_invalid_windows() {
    for window in ["0d", "all", "1x", "d", "366d", "53w"] {
        let ws = temp_ws(&format!(
            "args_explorer_index_window_{}",
            window.replace(['-', '.'], "_")
        ));
        let out = run(
            &ws,
            &["-f", &cfg(), "explorer", "index", "--window", window],
        );
        expect_fail(&out, &format!("explorer index --window {window}"));
        let combined = out.combined();
        assert!(
            combined.contains("invalid --window") || combined.contains("365-day limit"),
            "expected window validation for {window:?}, got:\n{combined}"
        );
    }
}

/// `--window` cannot be combined with exact block bounds.
#[test]
fn explorer_index_rejects_window_with_blocks() {
    let ws = temp_ws("args_explorer_index_window_blocks");
    let out = run(
        &ws,
        &[
            "-f",
            &cfg(),
            "explorer",
            "index",
            "--window",
            "1d",
            "--from-block",
            "1",
            "--to-block",
            "2",
        ],
    );
    expect_fail(&out, "explorer index --window --from-block");
    let combined = out.combined();
    assert!(
        combined.contains("cannot be used with") || combined.contains("conflict"),
        "expected clap range conflict, got:\n{combined}"
    );
}

/// Tip-follow flags were removed; `--loop` is an unknown argument on index.
#[test]
fn explorer_index_rejects_loop_flag() {
    let ws = temp_ws("args_explorer_index_loop");
    let out = run(&ws, &["-f", &cfg(), "explorer", "index", "--loop"]);
    expect_fail(&out, "explorer index --loop");
    let combined = out.combined();
    assert!(
        combined.contains("unexpected argument") || combined.contains("unrecognized"),
        "expected clap unknown-arg rejection for --loop, got:\n{combined}"
    );
}

#[test]
fn quiet_and_verbose_flags_parse_ok_offline() {
    let ws = temp_ws("args_quiet_verbose");
    let out = run(&ws, &["--quiet", "-f", &cfg(), "config"]);
    expect_ok(&out, "--quiet config");
    let out = run(&ws, &["--verbose", "-f", &cfg(), "config"]);
    expect_ok(&out, "--verbose config");
}

// â”€â”€ report positive selection with a hand-written SQLite fixture (offline) â”€â”€â”€

/// Seed a cache DB and an explorer DB in `ws/cache/` with two run manifests
/// and a couple of opportunity rows, so the offline `report` tests can run.
fn seed_report_fixture(ws: &Path) {
    use rusqlite::Connection;
    std::fs::create_dir_all(ws.join("cache")).unwrap();
    let cache_path = ws.join("cache/polygon-mev-scout.sqlite");
    let conn = Connection::open(&cache_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS run_manifests (
            run_id TEXT PRIMARY KEY,
            chain TEXT NOT NULL,
            start_block INTEGER NOT NULL,
            end_block INTEGER NOT NULL,
            resolved_at INTEGER NOT NULL,
            range_mode TEXT NOT NULL,
            strategies TEXT NOT NULL,
            flash_loan_provider TEXT NOT NULL
        );
        INSERT INTO run_manifests
            (run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider)
        VALUES
            ('run_1111111111', 'polygon', 50000000, 50000004, 1700000000, 'blocks', 'two_hop_arb', 'none'),
            ('run_2222222222', 'polygon', 60000000, 60000004, 1700000100, 'blocks', 'two_hop_arb', 'none');",
    )
    .unwrap();

    let explorer_path = ws.join("cache/explorer-polygon.sqlite");
    let econn = Connection::open(&explorer_path).unwrap();
    econn
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS opportunities(
                run_id TEXT,
                chain TEXT,
                block_number INTEGER NOT NULL,
                tx_index INTEGER,
                strategy TEXT NOT NULL,
                pool_a TEXT,
                pool_b TEXT,
                token_in TEXT,
                token_out TEXT,
                input_amount TEXT,
                expected_profit TEXT,
                gas_cost_wei TEXT,
                path TEXT,
                timestamp INTEGER,
                mempool_only INTEGER,
                confidence TEXT,
                sender TEXT,
                tx_hash TEXT,
                detection_path TEXT,
                canonical_id TEXT
            );
            INSERT INTO opportunities
                (run_id, chain, block_number, tx_index, strategy, pool_a, expected_profit, gas_cost_wei, timestamp)
            VALUES
                ('run_2222222222', 'polygon', 60000001, 10, 'two_hop_arb',
                 '0x0000000000000000000000000000000000000001', '500', '21000', 1700000100),
                ('run_2222222222', 'polygon', 60000003, 22, 'two_hop_arb',
                 '0x0000000000000000000000000000000000000002', '120', '42000', 1700000102);",
        )
        .unwrap();
}

#[test]
fn report_selects_explicit_run_id_offline() {
    let ws = temp_ws("args_report_pos");
    seed_report_fixture(&ws);

    // Pin the scanner + explorer DB paths so the fixture and CLI agree.
    let cache_db = ws
        .join("cache/polygon-mev-scout.sqlite")
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let explorer_db = ws
        .join("cache/explorer-polygon.sqlite")
        .to_str()
        .unwrap()
        .replace('\\', "/");

    // Single-chain config: the example template is a multi-chain registry,
    // and a shared explorer.db_path across chain sections is rejected at load
    // (explorer fact tables are not chain-keyed).
    let write_cfg = |ws: &std::path::Path, extras: &[(&str, &str)]| -> String {
        let mut body = String::from("chain = \"polygon\"\n");
        for (key, value) in extras {
            if value.starts_with('"') {
                body.push_str(&format!("{key} = {value}\n"));
            } else {
                body.push_str(&format!("{key} = \"{value}\"\n"));
            }
        }
        body.push_str(&format!("\n[explorer]\ndb_path = \"{explorer_db}\"\n"));
        let path = ws.join("cfg-single-chain.toml");
        std::fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_string()
    };

    let cfg_path = write_cfg(&ws, &[("output", "\"json\""), ("db_path", &cache_db)]);
    let out = run(
        &ws,
        &["-f", &cfg_path, "report", "--run-id", "run_1111111111"],
    );
    expect_ok(&out, "report --run-id explicit positive selection");
    let parsed: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).expect("report --output json must print pure JSON");
    assert_eq!(
        parsed["run_id"].as_str(),
        Some("run_1111111111"),
        "explicit --run-id must select that run"
    );

    // default (latest) picks run_2222222222 (higher resolved_at)
    let out = run(&ws, &["-f", &cfg_path, "report"]);
    expect_ok(&out, "report default (latest by resolved_at)");
    let parsed: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).expect("report --output json must print pure JSON");
    assert_eq!(
        parsed["run_id"].as_str(),
        Some("run_2222222222"),
        "default selection must pick the latest run"
    );

    // default table output
    let default_cfg = write_cfg(&ws, &[("db_path", &cache_db)]);
    let out = run(&ws, &["-f", &default_cfg, "report"]);
    expect_ok(&out, "report default table output");
    assert!(out.stdout.contains("Run ID:"), "table output lacks Run ID");

    // csv output
    let csv_cfg = write_cfg(&ws, &[("output", "\"csv\""), ("db_path", &cache_db)]);
    let out = run(&ws, &["-f", &csv_cfg, "report"]);
    expect_ok(&out, "report csv with opportunities");
    assert!(
        out.stdout.lines().any(|l| {
            l.trim()
                == "block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence"
        }),
        "csv header line missing:\n{}",
        out.stdout
    );
}
