//! Opt-in network coverage tests (فاز ۳ of docs/CLI_TESTS_REVIEW_AND_PLAN.md).
//!
//! All tests are gated behind `MEV_SCOUT_E2E=1` + RPC reachability and
//! serialized via `rpc_lock()`. Network flakiness is tolerated with
//! SKIP/WARN instead of hard failures, mirroring the other E2E binaries.
//!
//! Covers the gaps listed in بخش ۱.۳ of the review:
//! - `scan --kind` for transfers / flashloans / labels + CSV output
//! - `scan --address` filter subset + the silently-ignored invalid filter
//! - `discover` flags: `--source hybrid`, `--incremental`, `--health-check
//!   false`, `--solidly-fee-bps`, the `--batch-size > 5000` warning
//! - `validate-pools --source gecko --markdown-out`
//! - `run` / `fetch --batch-rpc` one-block smokes
//! - `fetch` idempotency over a fixed range (second run reports Cached:)
//! - `replay --tx-index 0`

mod common;

use common::{
    ensure_gate_and_rpc, expect_ok, extract_json_array, make_cfg, rpc_lock, run_timed, scout,
    HEAVY_TIMEOUT, NETWORK_TIMEOUT,
};
use std::time::Duration;

/// 10-minute cap for the whole-file heavy helpers (kept below HEAVY_TIMEOUT).
const EXTRA_HEAVY: Duration = Duration::from_secs(900);

fn tolerant(run: Result<common::TimedOutput, String>, ctx: &str) -> Option<common::TimedOutput> {
    match run {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("SKIP: {ctx} exceeded budget (public-RPC stall):\n{e}");
            None
        }
    }
}

#[test]
fn scan_kinds_labels_transfers_flashloans_and_outputs() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_scan") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    // labels — cheapest (bundled, local DB), only needs RPC up for init.
    let labels_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &labels_cfg,
        "scan",
        "--kind",
        "labels",
        "--blocks",
        "1",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan labels") {
        expect_ok(&out, "scan --kind labels");
        assert!(
            out.stdout.contains("address labels"),
            "labels scan should print the bundled label summary, got:\n{}",
            out.stdout
        );
    }

    // transfers — JSON output, structural field checks only.
    let json_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &json_cfg,
        "scan",
        "--kind",
        "transfers",
        "--blocks",
        "1",
        "--limit",
        "10",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan transfers json") {
        expect_ok(&out, "scan --kind transfers json");
        let events = extract_json_array(&out.stdout)
            .expect("transfers --output json should print a JSON array");
        let items = events.as_array().expect("transfers json must be an array");
        for e in items {
            assert!(e.get("block").is_some(), "transfer missing block: {e}");
            assert!(e.get("tx_hash").is_some(), "transfer missing tx_hash: {e}");
        }
    }

    // transfers — CSV output header.
    let csv_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"csv\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &csv_cfg,
        "scan",
        "--kind",
        "transfers",
        "--blocks",
        "1",
        "--limit",
        "5",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan transfers csv") {
        expect_ok(&out, "scan --kind transfers csv");
        assert!(
            out.stdout
                .lines()
                .any(|l| l.trim() == "block,tx_hash,token,from,to,value"),
            "transfers csv header line missing:\n{}",
            out.stdout
        );
    }

    // flashloans — may legitimately find zero events; structural only.
    // Recreate the json cfg: every make_cfg writes the same ws TOML file,
    // and the CSV step above has since overwritten `output`.
    let json_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &json_cfg,
        "scan",
        "--kind",
        "flashloans",
        "--blocks",
        "1",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan flashloans") {
        expect_ok(&out, "scan --kind flashloans");
        let events = extract_json_array(&out.stdout)
            .expect("flashloans --output json should print a JSON array");
        assert!(
            events.as_array().is_some(),
            "flashloans json must be an array"
        );
    }
}

#[test]
fn scan_address_filter_and_silent_invalid_filter() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_addr") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    // Discover a real pool address to filter on.
    let discover_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &discover_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "2",
        "--json",
    ]);
    let Some(out) = tolerant(
        run_timed(&mut c, HEAVY_TIMEOUT),
        "discover for address filter",
    ) else {
        return;
    };
    expect_ok(&out, "discover onchain 2 blocks");
    let pools = extract_json_array(&out.stdout).expect("discover json array");
    let Some(pool_addr) = pools
        .as_array()
        .and_then(|a| a.first())
        .and_then(|p| p.get("address"))
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
    else {
        eprintln!("SKIP: no pools discovered to derive an address filter from");
        return;
    };

    // Positive filter: a scan with --address must succeed and, when it
    // returns events, every event must be from that address subset.
    let scan_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &scan_cfg,
        "scan",
        "--kind",
        "trades",
        "--blocks",
        "2",
        "--address",
        &pool_addr,
        "--limit",
        "50",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan trades --address") {
        expect_ok(&out, "scan trades with pool address filter");
        let events = extract_json_array(&out.stdout)
            .expect("scan --address --output json should print a JSON array");
        let items = events
            .as_array()
            .expect("scan --output json must be an array");
        if items.is_empty() {
            eprintln!(
                "WARN: filtered scan returned 0 events — affinity check vacuous for this window"
            );
        }
        for e in items {
            let pool = e.get("pool").and_then(|p| p.as_str()).unwrap_or("");
            assert!(
                pool.eq_ignore_ascii_case(&pool_addr),
                "filtered scan returned foreign pool {pool} (expected {pool_addr})"
            );
        }
    }

    // Documented current behavior: an unparsable address is silently dropped
    // from the filter (scan.rs uses parse().ok() filter_map) — the scan runs
    // unfiltered instead of failing.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &scan_cfg,
        "scan",
        "--kind",
        "trades",
        "--blocks",
        "1",
        "--address",
        "notanaddress",
        "--limit",
        "5",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "scan invalid address") {
        expect_ok(
            &out,
            "scan with unparsable --address must not fail (documented silent drop)",
        );
    }
}

#[test]
fn discover_hybrid_incremental_and_flags() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    // Batch-size warning is printed for values above the 5000 recommendation.
    let warn_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &warn_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "1",
        "--batch-size",
        "6000",
        "--json",
    ]);
    if let Some(out) = tolerant(
        run_timed(&mut c, HEAVY_TIMEOUT),
        "discover batch-size warning",
    ) {
        expect_ok(&out, "discover with --batch-size 6000");
        assert!(
            out.combined().contains("exceeds recommended maximum"),
            "expected the >5000 batch-size warning, got:\n{}",
            out.combined()
        );
    }

    // Baseline onchain discover on a shared db, then incremental resume.
    // NB: `--health-check false` is intentionally NOT exercised here —
    // real CLI behavior (verified): clap's `default_value = "true"` flag
    // without `num_args`/`action` rejects both `--health-check false` and
    // `--health-check=false`, so disabling the health check is unreachable
    // from the command line. The default (enabled) path is covered instead.
    let base_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &base_cfg,
        "discover",
        "--source",
        "onchain",
        "--blocks",
        "2",
        "--solidly-fee-bps",
        "30",
        "--json",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover baseline") {
        expect_ok(&out, "discover onchain with --solidly-fee-bps 30");

        let mut c = scout(&ws);
        c.args([
            "-f",
            &base_cfg,
            "discover",
            "--incremental",
            "--blocks",
            "2",
            "--json",
        ]);
        if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "discover incremental") {
            expect_ok(&out, "discover --incremental after baseline");
        }
    }

    // hybrid — union of onchain + remote; tolerant to remote-side failures.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &base_cfg,
        "discover",
        "--source",
        "hybrid",
        "--max-pools",
        "20",
        "--json",
    ]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover hybrid") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("hybrid discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                eprintln!("hybrid discovery returned {} pools", entries.len());
                // dedup by address is the union contract.
                let addrs: Vec<_> = entries
                    .iter()
                    .filter_map(|p| p.get("address").and_then(|a| a.as_str()))
                    .collect();
                let unique: std::collections::HashSet<_> =
                    addrs.iter().map(|s| s.to_lowercase()).collect();
                assert_eq!(
                    addrs.len(),
                    unique.len(),
                    "hybrid union must dedup pools by address"
                );
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): hybrid discover failed (remote aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }
}

#[test]
fn discover_remote_option_flags_enrich_min_tvl_resolve_metadata() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_disc_opts") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();
    let cfg = make_cfg(&ws, &[("db_path", &db_s)]);

    // --enrich: attaches tvl_usd / volume_usd_24h / volume_usd_30d from
    // GeckoTerminal; implies one remote fetch, so tolerant to service failures.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--max-pools",
        "20",
        "--json",
    ]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover --enrich") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("enriched discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                eprintln!("enriched remote discovery returned {} pools", entries.len());
                for p in entries {
                    // Enrichment contract: the fields must exist (null when the
                    // aggregator had no data for that pool).
                    assert!(
                        p.get("tvl_usd").is_some() && p.get("volume_usd_24h").is_some(),
                        "enriched pool must carry tvl/volume fields: {p}"
                    );
                }
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --enrich discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }

    // --min-tvl: dust-suppression filter on remote-sourced pools. Smoke only —
    // every returned pool must respect the floor when tvl is present.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--min-tvl",
        "100000",
        "--max-pools",
        "20",
        "--json",
    ]);
    match tolerant(run_timed(&mut c, EXTRA_HEAVY), "discover --min-tvl") {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("min-tvl discover success must print a JSON array");
            if let Some(entries) = pools.as_array() {
                for p in entries {
                    if let Some(tvl) = p.get("tvl_usd").and_then(|v| v.as_f64()) {
                        assert!(
                            tvl >= 100_000.0,
                            "--min-tvl 100000 must filter pools below the floor, got {tvl}"
                        );
                    }
                }
            }
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --min-tvl discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }

    // --resolve-remote-metadata: Multicall3 batch filling fee/tickSpacing/token
    // metadata for remote CL pools; results persist to the SQLite cache.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "discover",
        "--source",
        "remote",
        "--enrich",
        "--resolve-remote-metadata",
        "--max-pools",
        "10",
        "--json",
    ]);
    match tolerant(
        run_timed(&mut c, EXTRA_HEAVY),
        "discover --resolve-remote-metadata",
    ) {
        Some(out) if out.success => {
            let pools = extract_json_array(&out.stdout)
                .expect("resolve-remote-metadata discover success must print a JSON array");
            assert!(pools.as_array().is_some(), "output must be an array");
        }
        Some(out) => eprintln!(
            "WARN (tolerant): --resolve-remote-metadata discover failed (aggregator side?)\n{}",
            out.combined()
        ),
        None => {}
    }
}

#[test]
fn validate_pools_gecko_markdown_out() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_vpools") else {
        return;
    };
    let md = ws.join("validation.md");
    let md_s = md.to_str().unwrap();

    let mut c = scout(&ws);
    c.args([
        "-f",
        &make_cfg(&ws, &[("db_path", ws.join("cache.db").to_str().unwrap())]),
        "validate-pools",
        "--days",
        "1",
        "--source",
        "gecko",
        "--markdown-out",
        md_s,
        "--json",
    ]);
    let Some(out) = tolerant(run_timed(&mut c, EXTRA_HEAVY), "validate-pools gecko") else {
        return;
    };
    if !out.success {
        eprintln!(
            "WARN (tolerant): validate-pools gecko failed (reference service-side?)\n{}",
            out.combined()
        );
        return;
    }
    assert!(
        md.exists(),
        "--markdown-out must write the markdown report file"
    );
    let content = std::fs::read_to_string(&md).unwrap();
    assert!(
        content.contains("| Source |"),
        "markdown report should contain the source table header:\n{}",
        content
    );
}

/// Parse `Resolved range: blocks X–Y (N blocks)` (en dash) into (X, Y).
fn parse_resolved_range(stdout: &str) -> Option<(String, String)> {
    let line = stdout
        .lines()
        .find(|l| l.contains("Resolved range: blocks "))?;
    let range = line.split("blocks ").nth(1)?.split(' ').next()?;
    let (a, b) = range.split_once('–')?;
    Some((a.trim().to_string(), b.trim().to_string()))
}

/// Parse the `Cached:       N` summary line into N.
fn parse_cached_count(stdout: &str) -> Option<u64> {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Cached:"))?;
    line.split(':').nth(1)?.trim().parse().ok()
}

#[test]
fn fetch_idempotency_cached_on_refetch() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_fetch") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();
    let cfg = make_cfg(&ws, &[("db_path", &db_s)]);

    // Warm the RPC (fetch --blocks would resolve a fresh tip on every pass,
    // so the idempotency proof below pins the exact range instead).
    let mut c = scout(&ws);
    c.args([
        "-f", &cfg, "scan", "--kind", "trades", "--blocks", "3", "--limit", "1",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "tip probe") {
        expect_ok(&out, "tip probe scan");
    }

    let mut c = scout(&ws);
    c.args(["-f", &cfg, "fetch", "--blocks", "3", "--no-sig-resolve"]);
    let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "fetch first pass") else {
        return;
    };
    expect_ok(&out, "fetch 3 blocks first pass");
    assert!(out.stdout.contains("Fetch complete:"));
    let (from, to) = parse_resolved_range(&out.stdout)
        .expect("fetch must print 'Resolved range: blocks X–Y (N blocks)'");

    // Second pass over the SAME pinned range must come entirely from cache.
    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "fetch",
        "--from-block",
        &from,
        "--to-block",
        &to,
        "--no-sig-resolve",
    ]);
    let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "fetch second pass") else {
        return;
    };
    expect_ok(&out, "fetch pinned range second pass");
    let cached = parse_cached_count(&out.stdout).expect("fetch summary must print 'Cached: N'");
    assert!(
        cached > 0,
        "refetching the exact same range must hit the cache, got Cached: {cached}"
    );
    let total: u64 = to.parse::<u64>().unwrap() - from.parse::<u64>().unwrap() + 1;
    if cached < total {
        eprintln!("WARN: refetch cached {cached}/{total} blocks — pass 1 left gaps");
    }
}

#[test]
fn batch_rpc_smokes_run_and_fetch() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_batch") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();

    let fetch_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
    let mut c = scout(&ws);
    c.args([
        "-f",
        &fetch_cfg,
        "fetch",
        "--batch-rpc",
        "--blocks",
        "2",
        "--no-sig-resolve",
    ]);
    if let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "fetch --batch-rpc") {
        expect_ok(&out, "fetch --batch-rpc 2 blocks");
        assert!(out.stdout.contains("Fetch complete:"));
    }

    let run_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c = scout(&ws);
    c.args(["-f", &run_cfg, "run", "--batch-rpc", "--blocks", "2"]);
    if let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "run --batch-rpc") {
        expect_ok(&out, "run --batch-rpc 2 blocks");
        // Verify history is in SQLite via report
        let report_cfg = make_cfg(&ws, &[("db_path", &db_s)]);
        let mut c2 = scout(&ws);
        c2.args(["-f", &report_cfg, "report"]);
        if let Some(out2) = tolerant(
            run_timed(&mut c2, common::TEST_TIMEOUT),
            "report after batch-rpc run",
        ) {
            expect_ok(&out2, "report after batch-rpc run");
            assert!(
                out2.stdout.contains("Run ID:"),
                "report should contain Run ID"
            );
        }
    }
}

#[test]
fn replay_tx_index_zero_smoke() {
    let _guard = rpc_lock();
    let Some(ws) = ensure_gate_and_rpc("netcov_replay") else {
        return;
    };
    let db_s = ws.join("cache.db").to_str().unwrap().to_string();
    let cfg = make_cfg(&ws, &[("db_path", &db_s)]);

    // replay reads from the cache — fetch a fixed single block first.
    let mut c = scout(&ws);
    c.args(["-f", &cfg, "fetch", "--blocks", "1", "--no-sig-resolve"]);
    let Some(out) = tolerant(run_timed(&mut c, NETWORK_TIMEOUT), "fetch for replay") else {
        return;
    };
    expect_ok(&out, "fetch 1 block for replay");

    let mut c = scout(&ws);
    c.args(["-f", &cfg, "run", "--blocks", "1"]);
    let Some(out) = tolerant(run_timed(&mut c, HEAVY_TIMEOUT), "run for replay block") else {
        return;
    };
    expect_ok(&out, "run for replay block");

    // Resolve the fetched block number from the run history in SQLite
    // via report --output json.
    let report_cfg = make_cfg(&ws, &[("db_path", &db_s), ("output", "\"json\"")]);
    let mut c2 = scout(&ws);
    c2.args(["-f", &report_cfg, "report"]);
    let report_out =
        run_timed(&mut c2, common::TEST_TIMEOUT).expect("report after run for replay block");
    let block: Option<u64> = report_out
        .stdout
        .lines()
        .next()
        .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .and_then(|v| v["start_block"].as_u64());
    let Some(block) = block else {
        eprintln!("SKIP: could not resolve a fetched block number for replay --tx-index");
        return;
    };

    let mut c = scout(&ws);
    c.args([
        "-f",
        &cfg,
        "replay",
        "--block",
        &block.to_string(),
        "--tx-index",
        "0",
        "--analyze",
    ]);
    let Some(out) = tolerant(run_timed(&mut c, EXTRA_HEAVY), "replay --tx-index 0") else {
        return;
    };
    expect_ok(&out, "replay --tx-index 0");
    if !out.stdout.contains("Receipt verification:") {
        eprintln!("WARN: no Receipt verification line for --tx-index 0 run");
    }
}
