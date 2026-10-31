//! Phase 4 offline FP check for the causal (backrun / frontrun) passes.
//!
//! The explorer seed cache (`cache/explorer-avalanche.sqlite`) is a fixed,
//! hand-hunted set of real Avalanche blocks (Phase 3 re-hunt 2026-10-24: 0
//! `frontrun`/`backrun`/`skim` ops in both the 95681722..=95682322 and
//! 98415954..=98416254 windows). Ground truth for this data: **zero real
//! causal ops**. Therefore every `Backrun`/`Frontrun` claim the *current*
//! causal classifier emits on it is, by construction, a false positive.
//!
//! This test reconstructs each swap-dense block (>= 2 swaps — the minimum a
//! causal pair needs) as a `BlockInput` and runs `classify_block`, the exact
//! path `ingest` takes, counting and asserting on the causal FP count. This is
//! the plan's "negative fixture" FP check: REVM `profit(B|after A)` is
//! infeasible here, because the raw block cache (`avalanche-mev-scout.sqlite`)
//! carries no `blocks`/`receipts`/`transactions` state (only `pool_info` /
//! `token_symbols`), so there is no state to execute against. That REVM
//! variant is a documented follow-up, not a regression of this gate.
//!
//! Reads the seeded explorer cache only; no RPC. Skips (no hard fail) when the
//! cache is absent, mirroring the RPC corpus gating.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::str::FromStr;

use alloy::primitives::{address, Address, B256, U256};
use rusqlite::Connection;

use mev_scout_core::explorer::profit::ProfitTokenPolicy;
use mev_scout_core::explorer::types::{Amm, LegSource, MevKind, SwapFact, TransferFact};
use mev_scout_core::explorer::{classify_block, BlockContext, BlockInput, TxInput};

// ── Avalanche C-Chain values (core/data/chains.toml) ──────────────────────────
const WAVAX: Address = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
const USDC: Address = address!("b97ef9ef8734c71904d8002f8b6bc66dd9c48a6e");
const USDC_E: Address = address!("9702230a8ea53601f5cd2dc00fdbc13d4df19497");
const USDT_E: Address = address!("c7198437980c041389c85c43e942b31037adb125");
const DAI_E: Address = address!("d586e7f844cea2f50bf48a84c291e3c71f0fda99");
const SAVAX: Address = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");

const SEED_DB: &str = "cache/explorer-avalanche.sqlite";
const MIN_SWAPS_PER_BLOCK: usize = 2;
/// Documented FP gate. The seed data has zero real causal ops, so the target is
/// a tight count; any claim is an FP by construction.
const CAUSAL_FP_GATE: usize = 0;

/// Raw `txs` row: `(hash, tx_index, block_number, from, to, success, gas_used,
/// effective_gas_price_gwei, value_native)`.
type TxRow = (
    String,
    i64,
    i64,
    String,
    Option<String>,
    bool,
    i64,
    f64,
    String,
);

/// Raw `swaps` row: `(block_number, tx_index, log_index, pool, amm, token_in,
/// token_out, amount_in, amount_out)`.
type SwapRow = (
    i64,
    i64,
    i64,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
);

fn seed_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(SEED_DB)
}

fn amm_from(amm: Option<&str>) -> Amm {
    match amm.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("v2") => Amm::V2,
        Some("v3") => Amm::V3,
        Some("v4") => Amm::V4,
        Some("infinity") => Amm::Infinity,
        Some("metric") => Amm::Metric,
        Some("fluid") => Amm::Fluid,
        Some("curve") => Amm::Curve,
        Some("balancer") => Amm::Balancer,
        Some("solidly") => Amm::Solidly,
        Some("lb") => Amm::Lb,
        Some("pendle") => Amm::Pendle,
        Some("aggregator") => Amm::Aggregator,
        _ => Amm::V2,
    }
}

#[inline(always)]
fn addr(s: &str) -> Address {
    Address::from_str(s).unwrap_or(Address::ZERO)
}
#[inline(always)]
fn u256(s: &str) -> U256 {
    U256::from_str(s).unwrap_or(U256::ZERO)
}

/// All facts for one block, keyed by block number, with tx-index sub-keys.
struct PerBlock {
    ts: u64,
    /// tx_index -> skeleton (no swaps/transfers merged yet).
    txs: HashMap<u64, TxInput>,
    /// tx_index -> that tx's swaps.
    swaps: HashMap<u64, Vec<SwapFact>>,
    /// tx_index -> that tx's transfers.
    transfers: HashMap<u64, Vec<TransferFact>>,
}

impl PerBlock {
    fn swap_count(&self) -> usize {
        self.swaps.values().map(|v| v.len()).sum()
    }
}

/// Reconstruct one block's classify input from its per-block facts.
fn block_input(block: u64, b: &PerBlock) -> BlockInput {
    // Preserve tx-index order so per-tx legs keep block order.
    let mut indices: Vec<u64> = b.txs.keys().copied().collect();
    indices.sort_unstable();
    let txs: Vec<TxInput> = indices
        .into_iter()
        .map(|i| {
            let mut t = b.txs[&i].clone();
            if let Some(s) = b.swaps.get(&i) {
                t.swaps = s.clone();
            }
            if let Some(tr) = b.transfers.get(&i) {
                t.transfers = tr.clone();
            }
            t
        })
        .collect();
    BlockInput {
        block,
        ts: b.ts,
        ctx: BlockContext {
            wrapped_native: WAVAX,
            profit_policy: ProfitTokenPolicy {
                priority: vec![USDC, USDC_E, USDT_E, DAI_E, WAVAX],
                wrapped_native: WAVAX,
                weth: WAVAX,
            },
            savax: Some(SAVAX),
            ..BlockContext::for_tests()
        },
        open_positions: vec![],
        interest_lookback: Default::default(),
        prior_oracle_answers: std::collections::HashMap::new(),
        savax_exchange_rate_wad: None,
        tip_sinks: std::collections::HashSet::new(),
        txs,
    }
}

#[test]
fn causal_classifier_offline_fp_gate() {
    let db_path = seed_path();
    if !db_path.exists() {
        eprintln!(
            "SKIP causal_fp_corpus: seed cache {} not found. Obtain it via \
             `explorer index` (Avalanche backfill) so the offline FP gate runs.",
            db_path.display()
        );
        return;
    }

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP causal_fp_corpus: cannot open seed cache: {e}");
            return;
        }
    };

    // ── block headers: block_number -> ts ───────────────────────────────────
    let ts_by_block: HashMap<u64, u64> = {
        let mut stmt = conn.prepare("SELECT block_number, ts FROM blocks").unwrap();
        let rows: Vec<(i64, i64)> = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.into_iter()
            .map(|(b, ts)| (b as u64, ts as u64))
            .collect()
    };

    // seed a PerBlock for every block, then fill.
    let mut blocks: HashMap<u64, PerBlock> = ts_by_block
        .iter()
        .map(|(b, ts)| {
            (
                *b,
                PerBlock {
                    ts: *ts,
                    txs: HashMap::new(),
                    swaps: HashMap::new(),
                    transfers: HashMap::new(),
                },
            )
        })
        .collect();

    // ── txs ─────────────────────────────────────────────────────────────────
    {
        let mut stmt = conn
            .prepare(
                "SELECT hash, tx_index, block_number, `from`, `to`, success, gas_used, \
                 effective_gas_price_gwei, value_native FROM txs",
            )
            .unwrap();
        let rows: Vec<TxRow> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for r in rows {
            let block = r.2 as u64;
            let tx = r.1 as u64;
            blocks
                .entry(block)
                .or_insert_with(|| PerBlock {
                    ts: 0,
                    txs: HashMap::new(),
                    swaps: HashMap::new(),
                    transfers: HashMap::new(),
                })
                .txs
                .insert(
                    tx,
                    TxInput {
                        tx_index: tx,
                        tx_hash: B256::from_str(&r.0).unwrap_or(B256::ZERO),
                        from: addr(r.3.as_str()),
                        to: r.4.map(|s| addr(s.as_str())),
                        success: r.5,
                        gas_used: r.6 as u64,
                        effective_gas_price_gwei: r.7,
                        value: u256(&r.8),
                        ..Default::default()
                    },
                );
        }
    }

    // ── swaps ───────────────────────────────────────────────────────────────
    {
        let mut stmt = conn
            .prepare(
                "SELECT block_number, tx_index, log_index, pool, amm, token_in, token_out, \
                 amount_in, amount_out FROM swaps",
            )
            .unwrap();
        let rows: Vec<SwapRow> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for r in rows {
            let block = r.0 as u64;
            let tx = r.1 as u64;
            blocks
                .entry(block)
                .or_insert_with(|| PerBlock {
                    ts: 0,
                    txs: HashMap::new(),
                    swaps: HashMap::new(),
                    transfers: HashMap::new(),
                })
                .swaps
                .entry(tx)
                .or_default()
                .push(SwapFact {
                    tx_index: tx,
                    log_index: r.2 as u64,
                    pool: addr(&r.3),
                    amm: amm_from(r.4.as_deref()),
                    token_in: addr(&r.5),
                    token_out: addr(&r.6),
                    amount_in: u256(&r.7),
                    amount_out: u256(&r.8),
                    token_source: LegSource::Registry,
                    tick: None,
                    owner: None,
                });
        }
    }

    // ── transfers ───────────────────────────────────────────────────────────
    {
        let mut stmt = conn
            .prepare(
                "SELECT block_number, tx_index, log_index, token, `from`, `to`, amount \
                 FROM transfers",
            )
            .unwrap();
        let rows: Vec<(i64, i64, i64, String, String, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for r in rows {
            let block = r.0 as u64;
            let tx = r.1 as u64;
            blocks
                .entry(block)
                .or_insert_with(|| PerBlock {
                    ts: 0,
                    txs: HashMap::new(),
                    swaps: HashMap::new(),
                    transfers: HashMap::new(),
                })
                .transfers
                .entry(tx)
                .or_default()
                .push(TransferFact {
                    tx_index: tx,
                    log_index: r.2 as u64,
                    token: addr(&r.3),
                    from: addr(&r.4),
                    to: addr(&r.5),
                    amount: u256(&r.6),
                });
        }
    }

    // ── run classify over swap-dense blocks ──────────────────────────────────
    let mut dense: Vec<u64> = blocks
        .keys()
        .filter(|bk| blocks[bk].swap_count() >= MIN_SWAPS_PER_BLOCK)
        .copied()
        .collect();
    dense.sort();

    let mut fp_events: Vec<(u64, u64, MevKind, Address)> = Vec::new(); // (block, tx_index, kind, searcher)
    for block in &dense {
        let input = block_input(*block, &blocks[block]);
        let classified = classify_block(&input);
        for e in &classified.events {
            if matches!(e.kind, MevKind::Backrun | MevKind::Frontrun) {
                fp_events.push((*block, e.tx_index, e.kind, e.searcher));
            }
        }
    }

    let fp_count = fp_events.len();
    eprintln!(
        "causal_fp_corpus: {} blocks total, {} swap-dense (>= {} swaps), \
         {} causal claims (Backrun/Frontrun)",
        blocks.len(),
        dense.len(),
        MIN_SWAPS_PER_BLOCK,
        fp_count
    );
    for (block, txi, kind, searcher) in fp_events.iter().take(25) {
        eprintln!("  FP {kind} @ block {block} tx {txi}: searcher {searcher}");
    }

    assert_eq!(
        fp_count, CAUSAL_FP_GATE,
        "causal FP gate: observed {fp_count} Backrun/Frontrun claims on zero-real-op \
         seed data; gate = {CAUSAL_FP_GATE}"
    );
}
