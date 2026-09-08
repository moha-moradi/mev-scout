mod common;

use common::{
    expect_fail, expect_ok, make_cfg, repo_config_str, run_timed, scout, temp_ws, TEST_TIMEOUT,
    TimedOutput,
};
use std::path::Path;
use std::time::Duration;

fn cfg() -> String {
    repo_config_str()
}

fn run(ws: &Path, args: &[&str]) -> TimedOutput {
    let mut c = scout(ws);
    c.args(args);
    run_timed(&mut c, TEST_TIMEOUT).expect("spawn/wait failed")
}

#[test]
fn help_lists_all_ten_commands() {
    let ws = temp_ws("args_help");
    let out = run(&ws, &["--help"]);
    expect_ok(&out, "mev-scout --help");
    for cmd in [
        "run", "fetch", "report", "config", "replay", "discover",
        "validate-pools", "tokens", "scan", "live", "explorer",
    ] {
        assert!(
            out.stdout.contains(cmd),
            "--help output missing subcommand '{cmd}'"
        );
    }
}

#[test]
fn explorer_help_lists_subcommands() {
    let ws = temp_ws("args_explorer_help");
    let out = run(&ws, &["explorer", "--help"]);
    expect_ok(&out, "explorer --help");
    for sub in [
        "doctor", "index", "live", "stats", "top", "show", "explain", "validate", "export",
    ] {
        assert!(
            out.stdout.contains(sub),
            "explorer --help missing subcommand '{sub}'"
        );
    }
}

#[test]
fn explorer_validate_fails_without_store() {
    // A workspace with no indexed explorer store must fail with a clear
    // message (no panic), not silently print an empty report.
    let ws = temp_ws("args_explorer_validate_empty");
    let out = run(&ws, &["explorer", "validate"]);
    expect_fail(&out, "explorer validate with empty store");
    assert!(
        out.combined().contains("explorer index") || out.combined().contains("no indexed ops"),
        "expected missing-store error, got:\n{}",
        out.combined()
    );
}

#[test]
fn explorer_top_rejects_bad_dimension() {
    let ws = temp_ws("args_explorer_top_bad");
    let out = run(&ws, &["explorer", "top", "--by", "nonsense"]);
    expect_fail(&out, "explorer top with bad --by");
}

#[test]
fn run_without_block_range_fails_offline() {
    let ws = temp_ws("args_run_norange");
    let out = run(&ws, &["run"]);
    expect_fail(&out, "run without block range");
    assert!(
        out.combined().contains("no block range specified"),
        "expected validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn fetch_without_block_range_fails_offline() {
    let ws = temp_ws("args_fetch_norange");
    let out = run(&ws, &["fetch"]);
    expect_fail(&out, "fetch without block range");
    assert!(
        out.combined().contains("no block range specified"),
        "expected validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn scan_without_block_range_fails_offline() {
    let ws = temp_ws("args_scan_norange");
    let out = run(&ws, &["scan"]);
    expect_fail(&out, "scan without block range");
    assert!(
        out.combined().contains("no block range specified"),
        "expected validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn days_above_365_rejected_by_clap() {
    let ws = temp_ws("args_days_400");
    let out = run(&ws, &["run", "--days", "400"]);
    expect_fail(&out, "run --days 400");
    assert!(
        out.stderr.contains("error") || out.stdout.contains("error"),
        "expected clap error output"
    );
}

#[test]
fn block_zero_rejected_by_clap() {
    let ws = temp_ws("args_block_0");
    let out = run(&ws, &["run", "--block", "0"]);
    expect_fail(&out, "run --block 0");
    assert!(
        out.stderr.contains("error") || out.stdout.contains("error"),
        "expected clap error output"
    );
}

#[test]
fn replay_requires_block_flag() {
    let ws = temp_ws("args_replay_noblock");
    let out = run(&ws, &["replay"]);
    expect_fail(&out, "replay without --block");
    assert!(
        out.stderr.to_lowercase().contains("required")
            || out.stderr.contains("--block"),
        "expected clap required-arg error, got: {}",
        out.stderr
    );
}

#[test]
fn unknown_subcommand_rejected() {
    let ws = temp_ws("args_unknown_cmd");
    let out = run(&ws, &["frobnicate"]);
    expect_fail(&out, "unknown subcommand");
}

#[test]
fn config_prints_resolved_toml_from_repo_file() {
    let ws = temp_ws("args_config");
    let out = run(&ws, &["-f", &cfg(), "config"]);
    expect_ok(&out, "config with repo mev-scout.toml");
    assert!(
        out.stdout.contains("polygon"),
        "config output should mention chain polygon"
    );
    // Dynamic provider count: compare against the repo TOML itself instead of
    // a hard-coded threshold, so adding/removing providers doesn't break the
    // test. The resolved config must include at least every committed URL.
    let repo_https_count = common::repo_config_text().matches("https://").count();
    assert!(
        repo_https_count >= 1,
        "repo mev-scout.toml should commit at least one provider"
    );
    let https_count = out.stdout.matches("https://").count();
    assert!(
        https_count >= repo_https_count,
        "config should resolve all {repo_https_count} committed providers, found {https_count} https URLs"
    );
}

#[test]
fn report_error_paths_fail_cleanly() {
    let ws = temp_ws("args_report_err");
    let missing = ws.join("nope");
    let missing_cfg = make_cfg(&ws, &[("export_path", missing.to_str().unwrap())]);
    let out = run(&ws, &["-f", &missing_cfg, "report"]);
    expect_fail(&out, "report on missing export dir");
    assert!(
        out.stderr.contains("does not exist"),
        "expected missing-dir error, got: {}",
        out.stderr
    );

    let empty = ws.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let empty_cfg = make_cfg(&ws, &[("export_path", empty.to_str().unwrap())]);
    let out = run(&ws, &["-f", &empty_cfg, "report"]);
    expect_fail(&out, "report on empty export dir");
    assert!(
        out.stderr.contains("no results files"),
        "expected no-results error, got: {}",
        out.stderr
    );

    let rid_cfg = make_cfg(&ws, &[("export_path", empty.to_str().unwrap())]);
    let out = run(
        &ws,
        &["-f", &rid_cfg, "report", "--run-id", "missing_run_id"],
    );
    expect_fail(&out, "report on nonexistent run id");
    assert!(
        out.stderr.contains("results file not found"),
        "expected file-not-found error, got: {}",
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

// ── Block-range validation (offline; validation.rs paths) ───────────────────

#[test]
fn from_block_without_to_block_rejected() {
    let ws = temp_ws("args_from_only");
    let out = run(&ws, &["run", "--from-block", "100"]);
    expect_fail(&out, "run --from-block without --to-block");
    assert!(
        out.combined().contains("must be used together"),
        "expected explicit validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn to_block_without_from_block_rejected() {
    let ws = temp_ws("args_to_only");
    let out = run(&ws, &["run", "--to-block", "100"]);
    expect_fail(&out, "run --to-block without --from-block");
    assert!(
        out.combined().contains("must be used together"),
        "expected explicit validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn to_block_lte_from_block_rejected() {
    let ws = temp_ws("args_reversed_range");
    let out = run(&ws, &["run", "--from-block", "200", "--to-block", "200"]);
    expect_fail(&out, "run with to_block == from_block");
    assert!(
        out.combined().contains("must be greater than"),
        "expected explicit validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn days_and_blocks_conflict_rejected() {
    let ws = temp_ws("args_days_blocks_conflict");
    let out = run(&ws, &["run", "--days", "2", "--blocks", "5"]);
    expect_fail(&out, "run --days 2 --blocks 5");
    assert!(
        out.combined().contains("cannot be used together"),
        "expected explicit validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn replay_rejects_days() {
    let ws = temp_ws("args_replay_days");
    let out = run(&ws, &["replay", "--days", "1"]);
    expect_fail(&out, "replay --days");
    // Real behavior: ReplayArgs has no range flags, so clap rejects --days
    // before the replay-specific validation.rs branch can be reached.
    assert!(
        out.combined().contains("unexpected argument '--days'"),
        "expected clap rejection of --days for replay, got:\n{}",
        out.combined()
    );
}

// ── Config file block-range fields (real behavior: CLI-only, ignored) ───────
//
// `Config.days/blocks/block/from_block/to_block` are `#[serde(skip)]`
// (core/src/config/settings.rs) — the TOML file can NOT set them. Values
// written to the config file are silently ignored, so `run` then fails with
// the generic "no block range specified" validation error. This test locks
// that real behavior; the fine-grained validation.rs branches (days bounds,
// blocks >= 1, block > 0, range order) are unreachable from both the CLI
// (clap value_parser rejects them first) and the config file (serde skip) —
// they only guard direct library use of core.

#[test]
fn config_file_block_range_fields_are_ignored_cli_only() {
    let ws = temp_ws("args_cfg_range_ignored");
    let cfg_path = make_cfg(
        &ws,
        &[
            ("days", "0"),
            ("blocks", "0"),
            ("block", "0"),
            ("from_block", "500"),
            ("to_block", "100"),
        ],
    );
    let out = run(&ws, &["-f", &cfg_path, "run"]);
    expect_fail(&out, "config file with invalid range values written to TOML");
    assert!(
        out.combined().contains("no block range specified"),
        "config-file range fields must be ignored (serde skip), yielding the \
         generic no-range validation error, got:\n{}",
        out.combined()
    );
}

// ── Config file loading behavior (verified real behavior: silent fallback) ──

#[test]
fn missing_config_file_falls_back_to_default() {
    let ws = temp_ws("args_cfg_missing");
    let out = run(&ws, &["-f", "definitely_missing_file.toml", "config"]);
    expect_ok(&out, "config with missing -f file (documented fallback to default)");
    assert!(
        out.stdout.contains("chain = \"polygon\""),
        "fallback default config should resolve the default chain, got:\n{}",
        out.stdout
    );
}

#[test]
fn broken_toml_config_file_falls_back_to_default() {
    let ws = temp_ws("args_cfg_broken");
    let broken = ws.join("broken.toml");
    std::fs::write(&broken, "not valid toml [[[").unwrap();
    let out = run(&ws, &["-f", broken.to_str().unwrap(), "config"]);
    expect_ok(&out, "config with broken TOML (documented fallback to default)");
    assert!(
        out.stdout.contains("chain = \"polygon\""),
        "fallback default config should resolve the default chain, got:\n{}",
        out.stdout
    );
}

#[test]
fn config_env_var_placeholders_expand_from_environment() {
    let ws = temp_ws("args_cfg_env_expand");
    let cfg_path = make_cfg(
        &ws,
        &[("rpc_urls", "[\"https://rpc.example/v2/${MS_E2E_TEST_KEY}\"]")],
    );

    // Without the variable set the placeholder stays verbatim (visible in the
    // resolved config) — a missing key never silently corrupts the URL.
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

// ── Global flags & misc offline behaviors ────────────────────────────────────

#[test]
fn version_flag_exits_zero_with_version() {
    let ws = temp_ws("args_version");
    let out = run(&ws, &["--version"]);
    expect_ok(&out, "mev-scout --version");
    assert!(
        out.stdout.trim().split_whitespace().count() >= 2
            && out.stdout.chars().any(|c| c.is_ascii_digit()),
        "--version should print '<name> <version>', got: {}",
        out.stdout
    );
}

#[test]
fn no_subcommand_fails_with_usage_error() {
    let ws = temp_ws("args_no_cmd");
    let out = run(&ws, &[]);
    expect_fail(&out, "invocation without a subcommand");
    assert!(
        out.stderr.contains("Usage") || out.stderr.contains("usage"),
        "expected clap usage error, got: {}",
        out.stderr
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

// ── tokens (fully offline: SQLite + bundled known-token list) ────────────────

#[test]
fn tokens_filters_work_offline() {
    let ws = temp_ws("args_tokens");
    let db = ws.join("tokens.db");
    let db_s: &str = db.to_str().unwrap();

    let base = make_cfg(&ws, &[("db_path", db_s)]);

    // Full unfiltered dump via JSON. NB: tracing INFO lines share stdout, so
    // the JSON array must be extracted rather than parsed whole.
    let json_cfg = make_cfg(&ws, &[("db_path", db_s), ("output", "\"json\"")]);
    let out = run(&ws, &["-f", &json_cfg, "tokens"]);
    expect_ok(&out, "tokens --output json (seeds cache offline)");
    let entries = common::extract_json_array(&out.stdout)
        .expect("tokens --output json should print a JSON array");
    let entries = entries.as_array().expect("tokens json must be an array");
    assert!(
        entries.len() >= 5,
        "bundled known-token list should seed the cache offline"
    );

    // Symbol substring filter (USDC is guaranteed in the bundled Polygon list).
    let out = run(&ws, &["-f", &json_cfg, "tokens", "--symbol", "USDC"]);
    expect_ok(&out, "tokens --symbol USDC");
    let entries = common::extract_json_array(&out.stdout)
        .expect("tokens --output json should print a JSON array");
    let entries = entries.as_array().expect("tokens json must be an array");
    assert!(
        !entries.is_empty()
            && entries
                .iter()
                .all(|t| t["symbol"].as_str().unwrap_or("").to_lowercase().contains("usdc")),
        "--symbol USDC must only keep USDC-like entries, got: {entries:?}"
    );

    // Exact decimals filter (USDC variants are 6-decimal).
    let out = run(&ws, &["-f", &json_cfg, "tokens", "--decimals", "6"]);
    expect_ok(&out, "tokens --decimals 6");
    let entries = common::extract_json_array(&out.stdout)
        .expect("tokens --output json should print a JSON array");
    let entries = entries.as_array().expect("tokens json must be an array");
    assert!(
        !entries.is_empty() && entries.iter().all(|t| t["decimals"] == 6),
        "--decimals 6 must only keep 6-decimal tokens, got: {entries:?}"
    );

    // Limit caps the listing.
    let out = run(&ws, &["-f", &json_cfg, "tokens", "--limit", "1"]);
    expect_ok(&out, "tokens --limit 1");
    let entries = common::extract_json_array(&out.stdout)
        .expect("tokens --output json should print a JSON array");
    assert!(
        entries.as_array().map(|a| a.len() <= 1).unwrap_or(false),
        "--limit 1 must cap the number of listed tokens, got: {entries:?}"
    );

    // Default human-readable table (rewrite base cfg: every make_cfg call
    // targets the same ws/mev-scout.toml path, last write wins).
    let base = make_cfg(&ws, &[("db_path", db_s)]);
    let out = run(&ws, &["-f", &base, "tokens"]);
    expect_ok(&out, "tokens default table output");
    assert!(
        out.stdout.contains("token(s) found"),
        "default table output should end with a token count line, got:\n{}",
        out.stdout
    );
    assert!(
        db.exists(),
        "tokens runs must persist their cache to the configured db_path"
    );
}

// ── report positive selection with a hand-written ResultsFile (offline) ─────

fn write_minimal_results_file(dir: &Path, run_id: &str) -> std::path::PathBuf {
    let path = dir.join(format!("{run_id}.json"));
    let json = serde_json::json!({
        "run_id": run_id,
        "chain": "polygon",
        "start_block": 50_000_000u64,
        "end_block": 50_000_004u64,
        "range_mode": "blocks",
        "strategies": ["two_hop_arb"],
        "flash_loan_provider": "none",
        "resolved_at": 0u64,
        "created_at": 0u64,
        "opportunities": [],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    path
}

#[test]
fn report_selects_explicit_run_id_offline() {
    let ws = temp_ws("args_report_pos");
    let export = ws.join("export");
    std::fs::create_dir_all(&export).unwrap();
    write_minimal_results_file(&export, "run_1111111111");
    std::thread::sleep(Duration::from_millis(50));
    write_minimal_results_file(&export, "run_2222222222");

    let cfg_path = make_cfg(
        &ws,
        &[("export_path", export.to_str().unwrap()), ("output", "\"json\"")],
    );

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
        "explicit --run-id must select that exact file"
    );

    let out = run(&ws, &["-f", &cfg_path, "report"]);
    expect_ok(&out, "report default (latest by created order)");
    let parsed: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).expect("report --output json must print pure JSON");
    assert_eq!(
        parsed["run_id"].as_str(),
        Some("run_2222222222"),
        "default selection must pick the newest results file"
    );

    let cfg_path = make_cfg(&ws, &[("export_path", export.to_str().unwrap())]);
    let out = run(&ws, &["-f", &cfg_path, "report"]);
    expect_ok(&out, "report default table output");
    assert!(
        out.stdout.contains("Run ID:"),
        "table output lacks Run ID"
    );

    let csv_cfg = make_cfg(
        &ws,
        &[("export_path", export.to_str().unwrap()), ("output", "\"csv\"")],
    );
    let out = run(&ws, &["-f", &csv_cfg, "report"]);
    expect_ok(&out, "report csv with empty opportunities");
    assert!(
        out.stdout.lines().any(|l| l.trim()
            == "block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence"),
        "csv header line missing:\n{}",
        out.stdout
    );
}
