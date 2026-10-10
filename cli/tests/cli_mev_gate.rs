//! `explorer show` MEV gate coverage (MEV-VERIFICATION §A, step 5).
//!
//! The §A verdict (`mev_verdict`) has offline unit tests in the core crate, and
//! the real-block detector corpus drives the same function end-to-end. Neither
//! covers the **per-tx CLI path** — the `opportunities_for_tx` join, the printed
//! `mev gate:` line, and the `anyhow::bail!` that turns a `Fail` into a
//! non-zero exit.
//!
//! That path is deterministic given a seeded store, so this binary seeds one
//! directly (`insert_block_facts` for the realized op + `insert_opportunity`
//! for the detector row, joined on `tx_hash`) instead of gating on live RPC:
//! a verdict that only ever runs against a moving chain tip cannot be asserted,
//! only smoke-tested. Each test pins the two sides so the only variable is the
//! tolerance, which makes the three verdicts — `Pass`, `Fail` (non-zero exit),
//! `Unverifiable` (exit zero, warning) — separately reachable.
//!
//! `--trace` is deliberately not used: it needs a `debug_traceTransaction`
//! provider, and the realized side is already in the seeded `details_json`,
//! which is exactly the field `job_trace_op` would have written.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;

use alloy::primitives::{address, Address, B256, U256};

use common::{expect_ok, make_cfg, run_timed, scout, temp_ws, TEST_TIMEOUT};
use mev_scout_core::explorer::store::{BlockFactsInput, ExplorerStore, OpportunityInput};
use mev_scout_core::explorer::{Confidence, MevEvent, MevKind};

/// One native unit, as `i128`-friendly wei. Detector `expected_profit` /
/// `gas_cost_wei` and the realized `trace_native_delta_wei` are all wei.
const ONE: u128 = 1_000_000_000_000_000_000;

const BLOCK: u64 = 100;
const TS: u64 = 1_700_000_000;
const SEARCHER: Address = address!("00000000000000000000000000000000000000aa");
const POOL: Address = address!("00000000000000000000000000000000000000bb");

fn tx_hash(n: u8) -> B256 {
    let mut h = [0u8; 32];
    h[31] = n;
    h.into()
}

/// A store holding realized ops whose `details_json` carries `delta_wei` /
/// `profit_usd` (the fields `job_trace_op` writes and the §A gate reads), and
/// one detector opportunity. Returns the tx hash to `show`.
///
/// `ops` is one `(trace_native_delta_wei, trace_profit_usd)` entry per realized
/// op, in `ops_for_tx` order. A `None` delta omits the field — the
/// degraded-coverage case where the trace job never ran for that op.
///
/// The pair is the whole native-price evidence: the gate divides `usd` by
/// `|delta|` to recover a native price (`show.rs::native_price_usd`), which
/// sets the absolute wei band. Passing `(delta, usd)` lets a test place native
/// at a price other than $1, which is what makes the recovered price
/// distinguishable from the $1/native fallback.
fn seed(
    ws: &std::path::Path,
    n: u8,
    expected: u128,
    gas: u128,
    ops: &[(Option<i128>, f64)],
) -> String {
    let db_s = ws.join("explorer.sqlite").to_string_lossy().into_owned();
    let store = ExplorerStore::open(&db_s).expect("seed store opens");
    let hash = tx_hash(n);

    let events: Vec<MevEvent> = ops
        .iter()
        .enumerate()
        .map(|(tx_index, (delta, usd))| {
            let mut details = serde_json::json!({ "trace_profit_usd": usd });
            if let Some(w) = delta {
                details["trace_native_delta_wei"] = serde_json::json!(w.to_string());
            }
            MevEvent {
                block: BLOCK,
                ts: TS,
                tx_index: tx_index as u64,
                tx_hash: hash,
                kind: MevKind::ArbAtomic,
                searcher: SEARCHER,
                contract: None,
                pools: vec![POOL],
                profit_token: None,
                profit_amount: None,
                profit_tokens: Vec::new(),
                profit_usd: Some(*usd),
                gas_cost_wei: U256::from(gas),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Exact,
                victim_hashes: Vec::new(),
                victim_swap_size: None,
                details,
            }
        })
        .collect();
    store
        .insert_block_facts(BlockFactsInput {
            block_number: BLOCK,
            block_hash: &hash,
            ts: TS,
            base_fee_gwei: Some(0.05),
            tx_count: events.len(),
            txs: &[],
            swaps: &[],
            transfers: &[],
            events: &events,
            native_price_usd: Some(1.0),
            token_prices: &HashMap::new(),
        })
        .expect("seed realized ops");

    store
        .insert_opportunity(OpportunityInput {
            run_id: "run_mev_gate",
            chain: "avalanche",
            block_number: BLOCK,
            tx_index: Some(0),
            strategy: "two_hop_arb",
            pool_a: Some(POOL),
            pool_b: None,
            token_in: None,
            token_out: None,
            input_amount: None,
            expected_profit: Some(U256::from(expected)),
            gas_cost_wei: Some(U256::from(gas)),
            path: None,
            timestamp: Some(TS),
            mempool_only: false,
            confidence: Some("high"),
            sender: Some(SEARCHER),
            tx_hash: Some(hash),
            detection_path: Some("replay"),
            canonical_id: Some("TwoHopArb|seeded|0xbb|0xaa"),
        })
        .expect("seed detector opportunity");

    format!("{hash:#x}")
}

/// `explorer show <tx>` against the seeded store, with the MEV-gate tolerance
/// supplied through `[explorer] mev_tolerance_pct` (the `--tolerance-pct` flag
/// was removed). The config is rebuilt per call because `make_cfg` always
/// writes the same path.
fn show(ws: &std::path::Path, db_s: &str, hash: &str, tol: Option<&str>) -> common::TimedOutput {
    let cfg = make_cfg(ws, &[]);
    let mut f = OpenOptions::new().append(true).open(&cfg).unwrap();
    // Windows temp paths carry backslashes, which TOML reads as escapes.
    writeln!(
        f,
        "\n[explorer]\ndb_path = \"{}\"",
        db_s.replace('\\', "\\\\")
    )
    .unwrap();
    if let Some(pct) = tol {
        writeln!(f, "mev_tolerance_pct = {pct}").unwrap();
    }
    drop(f);

    let mut c = scout(ws);
    c.args(["-f", &cfg, "explorer", "show", hash]);
    run_timed(&mut c, TEST_TIMEOUT).expect("explorer show spawns")
}

/// The gate line the CLI prints, e.g.
/// `  mev gate: pass | expected 1900000000000000000 wei | realized 1000000000000000000 wei | tol ±100.0%`.
fn gate_line(out: &common::TimedOutput) -> String {
    out.combined()
        .lines()
        .find(|l| l.contains("mev gate:"))
        .unwrap_or_else(|| panic!("no `mev gate:` line printed:\n{}", out.combined()))
        .trim()
        .to_string()
}

/// §A: the tolerance decides the verdict, and `Fail` must fail the command
/// while `Pass` must not. Detector net 1.9 native vs realized 1.0 native is a
/// +47.4% over-estimate, so a ±100% band passes and a ±10% band does not.
#[test]
fn mev_gate_verdict_follows_tolerance_and_fail_exits_non_zero() {
    let ws = temp_ws("mev_gate_tol");
    let db_s = ws.join("explorer.sqlite").to_string_lossy().into_owned();
    let hash = seed(&ws, 1, 2 * ONE, ONE / 10, &[(Some(ONE as i128), 1.0)]);

    let passing = show(&ws, &db_s, &hash, Some("100"));
    let line = gate_line(&passing);
    assert!(
        line.contains("mev gate: pass"),
        "a +47.4% error inside a ±100% band must pass:\n{line}"
    );
    assert!(
        line.contains("expected Some(1900000000000000000) wei"),
        "gate line must report the detector net (expected_profit − gas_cost_wei):\n{line}"
    );
    assert!(
        line.contains("realized Some(1000000000000000000) wei"),
        "gate line must report the realized trace delta:\n{line}"
    );
    expect_ok(&passing, "explorer show with a passing MEV gate");

    let failing = show(&ws, &db_s, &hash, Some("10"));
    let line = gate_line(&failing);
    assert!(
        line.contains("mev gate: fail"),
        "the same pair outside a ±10% band must fail:\n{line}"
    );
    assert!(
        failing.combined().contains("over-estimate"),
        "a positive error must be classified as an over-estimate:\n{}",
        failing.combined()
    );
    assert!(
        !failing.success,
        "a failed MEV gate must exit non-zero\n--- stdout ---\n{}",
        failing.stdout
    );
    assert!(
        failing.stderr.contains("MEV gate FAILED"),
        "the bail must name the gate:\n--- stderr ---\n{}",
        failing.stderr
    );
}

/// §A: `Unverifiable` is degraded coverage, not a failure. With no
/// `trace_native_delta_wei` on the op there is no realized figure to compare
/// against, so the command warns and still exits zero.
#[test]
fn mev_gate_unverifiable_without_realized_delta_warns_and_exits_zero() {
    let ws = temp_ws("mev_gate_unverifiable");
    let db_s = ws.join("explorer.sqlite").to_string_lossy().into_owned();
    let hash = seed(&ws, 2, 2 * ONE, ONE / 10, &[(None, 1.0)]);

    // No explicit tolerance: this must read the config default.
    let out = show(&ws, &db_s, &hash, None);
    let line = gate_line(&out);
    assert!(
        line.contains("mev gate: unverifiable"),
        "a missing realized delta must degrade to unverifiable:\n{line}"
    );
    assert!(
        out.stderr.contains("MEV check unverifiable"),
        "unverifiable must warn on stderr:\n--- stderr ---\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("MEV gate FAILED"),
        "unverifiable must not be reported as a gate failure:\n--- stderr ---\n{}",
        out.stderr
    );
    expect_ok(&out, "explorer show with an unverifiable MEV gate");
}

/// §A: when the detector's expected net is ~0 the percentage band is
/// meaningless, so the gate falls back to the absolute wei band
/// (`mev_error_usd_tol`). The delta's `trace_profit_usd` puts native at $1, so
/// the band is 0.50 native: a 1-native gap fails, a 0.1 gap passes.
#[test]
fn mev_gate_uses_absolute_band_when_expected_net_is_zero() {
    let ws = temp_ws("mev_gate_abs_band");
    let db_s = ws.join("explorer.sqlite").to_string_lossy().into_owned();
    // expected_profit == gas_cost_wei ⇒ expected net is exactly 0.
    let over = seed(&ws, 3, ONE, ONE, &[(Some(ONE as i128), 1.0)]);
    let within = seed(&ws, 4, ONE, ONE, &[(Some((ONE / 10) as i128), 0.1)]);

    let out = show(&ws, &db_s, &over, Some("100"));
    assert!(
        gate_line(&out).contains("mev gate: fail"),
        "a 1-native gap must fail a 0.50-native band regardless of tolerance:\n{}",
        out.combined()
    );
    assert!(
        !out.success,
        "the absolute-band failure must exit non-zero\n--- stdout ---\n{}",
        out.stdout
    );

    let out = show(&ws, &db_s, &within, Some("100"));
    assert!(
        gate_line(&out).contains("mev gate: pass"),
        "a 0.1-native gap is inside the 0.50-native band:\n{}",
        out.combined()
    );
    expect_ok(&out, "explorer show with a within-band zero-expected net");
}

/// The absolute band's width comes from the native price recovered off the
/// first op carrying *both* trace fields. A multi-op tx whose leading op
/// predates the trace job must not knock the gate back to the $1/native
/// fallback, which silently rescales the band.
///
/// Op 0 has no `trace_native_delta_wei`; op 1's delta is 0.1 native recorded at
/// $10, so native is $100 and the true band is `0.50 / 100` = 0.005 native —
/// far below the 0.1-native gap, so this must fail. Falling back to
/// `mev_error_usd_tol * 1e18` (a $1/native band of 0.50 native) would instead
/// pass, so this test discriminates the two.
#[test]
fn mev_gate_recovers_native_price_past_an_untraced_leading_op() {
    let ws = temp_ws("mev_gate_multiop");
    let db_s = ws.join("explorer.sqlite").to_string_lossy().into_owned();
    let hash = seed(
        &ws,
        5,
        ONE,
        ONE,
        &[(None, 1.0), (Some((ONE / 10) as i128), 10.0)],
    );

    let out = show(&ws, &db_s, &hash, Some("100"));
    let line = gate_line(&out);
    assert!(
        line.contains("mev gate: fail"),
        "the band must come from the traced op ($100/native ⇒ 0.005 native), not the $1/native \
         fallback that would swallow the gap:\n{line}"
    );
    assert!(
        line.contains("5000000000000000 wei"),
        "the band must be 0.005 native (5e15 wei), not the fallback's 0.50 native:\n{line}"
    );
    assert!(
        !out.success,
        "the absolute-band failure must exit non-zero\n--- stdout ---\n{}",
        out.stdout
    );
}
