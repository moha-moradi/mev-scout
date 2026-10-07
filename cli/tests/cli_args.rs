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
    for sub in ["index", "show"] {
        assert!(
            help_lists_subcommand(&out.stdout, sub),
            "explorer --help missing subcommand '{sub}'"
        );
    }
    // Revenue report is bare `explorer`; `backfill` folded into `index`.
    for removed in [
        "report",
        "backfill",
        "stats",
        "doctor",
        "live-feed",
        "top",
        "explain",
        "export",
    ] {
        assert!(
            !help_lists_subcommand(&out.stdout, removed),
            "explorer --help still lists removed subcommand '{removed}'"
        );
    }
    for flag in ["--windows", "--kind", "--top"] {
        assert!(
            out.stdout.contains(flag),
            "explorer --help missing report flag '{flag}'"
        );
    }

    // `validate` is gated behind the non-default `validate` cargo feature: it
    // must be absent by default and present once the feature is compiled in.
    if cfg!(feature = "validate") {
        assert!(
            help_lists_subcommand(&out.stdout, "validate"),
            "explorer --help must list 'validate' under --features validate"
        );
    } else {
        assert!(
            !help_lists_subcommand(&out.stdout, "validate"),
            "explorer --help must hide 'validate' unless --features validate"
        );
    }
}

#[test]
fn live_help_lists_ledger_flags() {
    let ws = temp_ws("args_live_ledger_help");
    let out = run(&ws, &["live", "--help"]);
    expect_ok(&out, "live --help");
    for flag in ["--initial-balance", "--reserve"] {
        assert!(
            out.stdout.contains(flag),
            "live --help missing ledger flag '{flag}'"
        );
    }
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
fn removed_tokens_subcommand_fails() {
    let ws = temp_ws("args_tokens_removed");
    let out = run(&ws, &["tokens"]);
    expect_fail(&out, "removed tokens subcommand");
    assert!(
        out.combined().contains("unrecognized subcommand 'tokens'"),
        "expected clap to reject 'tokens', got:\n{}",
        out.combined()
    );
}

#[test]
fn explorer_removed_stats_subcommand_fails() {
    let ws = temp_ws("args_explorer_stats_removed");
    let out = run(&ws, &["explorer", "stats"]);
    expect_fail(&out, "removed explorer stats subcommand");
}

#[test]
fn explorer_removed_report_subcommand_fails() {
    let ws = temp_ws("args_explorer_report_removed");
    let out = run(&ws, &["explorer", "report"]);
    expect_fail(&out, "removed explorer report subcommand");
}

#[test]
fn explorer_removed_backfill_subcommand_fails() {
    let ws = temp_ws("args_explorer_backfill_removed");
    let out = run(&ws, &["explorer", "backfill"]);
    expect_fail(&out, "removed explorer backfill subcommand");
}

/// `--tolerance-pct` moved to `[explorer]` config, so the flag must be gone.
#[test]
fn explorer_show_rejects_tolerance_pct() {
    let ws = temp_ws("args_show_tolerance_removed");
    let out = run(&ws, &["explorer", "show", "0xabc", "--tolerance-pct", "10"]);
    expect_fail(&out, "explorer show --tolerance-pct");
}

#[test]
fn removed_paper_subcommand_fails() {
    let ws = temp_ws("args_paper_removed");
    let out = run(&ws, &["paper"]);
    expect_fail(&out, "removed paper subcommand");
    assert!(
        out.combined().contains("unrecognized subcommand 'paper'"),
        "expected clap to reject 'paper', got:\n{}",
        out.combined()
    );
}

#[test]
fn explorer_unknown_subcommand_fails() {
    let ws = temp_ws("args_explorer_unknown_sub");
    let out = run(&ws, &["explorer", "frobnicate"]);
    expect_fail(&out, "unknown explorer subcommand");
}

#[test]
fn explorer_removed_top_subcommand_fails() {
    let ws = temp_ws("args_explorer_top_removed");
    let out = run(&ws, &["explorer", "top", "--by", "sender"]);
    expect_fail(&out, "removed explorer top subcommand");
}

#[test]
fn removed_run_subcommand_fails() {
    let ws = temp_ws("args_run_removed");
    let out = run(&ws, &["run"]);
    expect_fail(&out, "removed run subcommand");
    assert!(
        out.combined().contains("unrecognized subcommand 'run'"),
        "expected clap to reject 'run', got:\n{}",
        out.combined()
    );
}

#[test]
fn removed_fetch_subcommand_fails() {
    let ws = temp_ws("args_fetch_removed");
    let out = run(&ws, &["fetch"]);
    expect_fail(&out, "removed fetch subcommand");
}

#[test]
fn removed_scan_subcommand_fails() {
    let ws = temp_ws("args_scan_removed");
    let out = run(&ws, &["scan"]);
    expect_fail(&out, "removed scan subcommand");
}

#[test]
fn removed_replay_subcommand_fails() {
    let ws = temp_ws("args_replay_removed");
    let out = run(&ws, &["replay"]);
    expect_fail(&out, "removed replay subcommand");
}

#[test]
fn unknown_subcommand_rejected() {
    let ws = temp_ws("args_unknown_cmd");
    let out = run(&ws, &["frobnicate"]);
    expect_fail(&out, "unknown subcommand");
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
    let example_https_count = common::example_config_text().matches("https://").count();
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

/// Bare `explorer` is the revenue report; report flags resolve their defaults.
#[test]
fn bare_explorer_is_revenue_report() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command, DEFAULT_EXPLORER_TOP, DEFAULT_EXPLORER_WINDOWS};

    let cli = Cli::parse_from(["mev-scout", "explorer"]);
    match cli.command {
        Some(Command::Explorer(a)) => {
            assert!(a.command.is_none(), "bare explorer has no subcommand");
            assert_eq!(a.resolved_windows(), DEFAULT_EXPLORER_WINDOWS);
            assert_eq!(a.resolved_top(), DEFAULT_EXPLORER_TOP);
            assert!(!a.report_flags_set());
        }
        other => panic!("expected Explorer, got {other:?}"),
    }

    let cli = Cli::parse_from(["mev-scout", "explorer", "--windows", "7d", "--top", "20"]);
    match cli.command {
        Some(Command::Explorer(a)) => {
            assert!(a.command.is_none());
            assert!(a.report_flags_set());
            assert_eq!(a.resolved_windows(), vec!["7d".to_string()]);
            assert_eq!(a.resolved_top(), 20);
        }
        other => panic!("expected Explorer, got {other:?}"),
    }
}

/// `explorer index` without `--loop` is a bounded backfill (default 7 days).
#[test]
fn explorer_index_defaults_to_backfill_shape() {
    use clap::Parser;
    use mev_scout_cli::cli::{Cli, Command, ExplorerCommand};

    let cli = Cli::parse_from(["mev-scout", "explorer", "index"]);
    match cli.command {
        Some(Command::Explorer(a)) => match a.command {
            Some(ExplorerCommand::Index(i)) => {
                assert!(!i.r#loop);
                assert!(i.duration.is_none());
                assert!(i.days.is_none());
                assert!(i.from_block.is_none());
                assert!(i.to_block.is_none());
            }
            other => panic!("expected Index, got {other:?}"),
        },
        other => panic!("expected Explorer, got {other:?}"),
    }

    let cli = Cli::parse_from([
        "mev-scout",
        "explorer",
        "index",
        "--loop",
        "--duration",
        "15m",
    ]);
    match cli.command {
        Some(Command::Explorer(a)) => match a.command {
            Some(ExplorerCommand::Index(i)) => {
                assert!(i.r#loop);
                assert_eq!(i.duration.as_deref(), Some("15m"));
            }
            other => panic!("expected Index, got {other:?}"),
        },
        other => panic!("expected Explorer, got {other:?}"),
    }
}

/// `--loop` + `--days` is rejected at dispatch (after clap parse).
#[test]
fn explorer_index_rejects_loop_with_days() {
    let ws = temp_ws("args_explorer_index_loop_days");
    let out = run(
        &ws,
        &["-f", &cfg(), "explorer", "index", "--loop", "--days", "7"],
    );
    expect_fail(&out, "explorer index --loop --days");
    assert!(
        out.combined().contains("cannot be combined with --loop"),
        "expected loop/days conflict message, got:\n{}",
        out.combined()
    );
}

/// Report flags on a subcommand are rejected.
#[test]
fn explorer_show_rejects_report_flags() {
    let ws = temp_ws("args_explorer_show_windows");
    let out = run(
        &ws,
        &["-f", &cfg(), "explorer", "--windows", "7d", "show", "0xabc"],
    );
    expect_fail(&out, "explorer --windows show");
    assert!(
        out.combined().contains("belong on bare `explorer`")
            || out.combined().contains("unexpected argument")
            || out.combined().contains("unrecognized"),
        "expected report-flag conflict or clap rejection, got:\n{}",
        out.combined()
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

    let write_cfg = |ws: &std::path::Path, extras: &[(&str, &str)]| -> String {
        let path = make_cfg(ws, extras);
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "\n[explorer]\ndb_path = \"{explorer_db}\"").unwrap();
        path
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

// ── explorer validate is hidden behind a non-default cargo feature ───────────
//
// The positive coverage for `validate` lives in `cli_explorer_validate.rs`,
// compiled only with `--features validate`.

// `validate` must be invisible in a default build. Under `--features validate`
// the subcommand legitimately exists, so the absence assertion does not apply;
// the positive coverage lives in `cli_explorer_validate.rs`.
#[cfg(not(feature = "validate"))]
#[test]
fn explorer_validate_is_hidden_by_default() {
    let ws = temp_ws("args_validate_gated");
    let out = run(
        &ws,
        &["-f", &make_cfg(&ws, &[]), "explorer", "validate", "--json"],
    );
    expect_fail(&out, "explorer validate without the `validate` feature");
    assert!(
        out.combined()
            .contains("unrecognized subcommand 'validate'"),
        "expected clap to reject 'validate' in the default build, got:\n{}",
        out.combined()
    );
}
