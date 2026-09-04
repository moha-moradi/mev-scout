mod common;

use common::{
    expect_fail, expect_ok, repo_config, run_timed, scout, temp_config, temp_ws, TEST_TIMEOUT,
    TimedOutput,
};
use std::path::Path;
use std::time::Duration;

fn cfg() -> String {
    repo_config().to_str().unwrap().to_string()
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
        "validate-pools", "tokens", "scan", "live",
    ] {
        assert!(
            out.stdout.contains(cmd),
            "--help output missing subcommand '{cmd}'"
        );
    }
}

#[test]
fn run_without_block_range_fails_offline() {
    let ws = temp_ws("args_run_norange");
    let out = run(&ws, &["run"]);
    expect_fail(&out, "run without block range");
}

#[test]
fn fetch_without_block_range_fails_offline() {
    let ws = temp_ws("args_fetch_norange");
    let out = run(&ws, &["fetch"]);
    expect_fail(&out, "fetch without block range");
}

#[test]
fn scan_without_block_range_fails_offline() {
    let ws = temp_ws("args_scan_norange");
    let out = run(&ws, &["scan"]);
    expect_fail(&out, "scan without block range");
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
    let https_count = out.stdout.matches("https://").count();
    assert!(
        https_count >= 9,
        "config should resolve the 9 committed providers, found {https_count} https URLs"
    );
}

#[test]
fn live_duration_without_loop_rejected_offline() {
    let ws = temp_ws("args_live_dur");
    let out = run(&ws, &["-f", &cfg(), "live", "--duration", "30s"]);
    expect_fail(&out, "live --duration without --loop");
    assert!(
        out.combined().contains("--duration requires --loop"),
        "expected explicit validation error, got:\n{}",
        out.combined()
    );
}

#[test]
fn report_error_paths_fail_cleanly() {
    let ws = temp_ws("args_report_err");
    let missing = ws.join("nope");
    let missing_cfg = temp_config(&ws, &[("export_path", missing.to_str().unwrap())]);
    let out = run(&ws, &["-f", missing_cfg.to_str().unwrap(), "report"]);
    expect_fail(&out, "report on missing export dir");
    assert!(
        out.stderr.contains("does not exist"),
        "expected missing-dir error, got: {}",
        out.stderr
    );

    let empty = ws.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let empty_cfg = temp_config(&ws, &[("export_path", empty.to_str().unwrap())]);
    let out = run(&ws, &["-f", empty_cfg.to_str().unwrap(), "report"]);
    expect_fail(&out, "report on empty export dir");
    assert!(
        out.stderr.contains("no results files"),
        "expected no-results error, got: {}",
        out.stderr
    );

    let rid_cfg = temp_config(&ws, &[("export_path", empty.to_str().unwrap())]);
    let out = run(
        &ws,
        &["-f", rid_cfg.to_str().unwrap(), "report", "--run-id", "missing_run_id"],
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
