//! Real-block golden corpus (WS-H2, first slice).
//!
//! Re-ingests fixed historical windows from a live RPC and asserts derived
//! facts (kind present, op counts above a floor, searcher identity, USD
//! profit floor). Expected numbers were collected with `explorer index`
//! and are not exact — thresholds sit near 60-70% of what that run stored.
//!
//! Avalanche seed (`cache/explorer-avalanche.sqlite`, blocks 95681722..=95682322):
//! - `arb_atomic` total: 630 ops
//! - `0x93a9f59d5defaae72702dedc2fc4a8fbc287a1ac`: 23 ops, max profit \$2332
//! - `0x92d4ee32bc0f81dbb9923073395a2be7febbc3e5`: 294 ops (dust, max \$0.28)
//! - block 95682057: 9 `arb_atomic` ops, max profit \$682
//! - block 95682033: 1 liquidation by `0xd2a82f1bb41a950ad24829b2f483b1b10f3569dd`
//!
//! Polygon seed (backfill 94430000..=94430280 and block 94430973, 2026-09-25):
//! - `arb_atomic`: 506 ops in the 281-block window
//! - `0x8695330488513a6c3698a2b072ff88aaedbfac3e`: 9 ops, max profit \$62
//! - `0x8c771167beba08d15acd9bee460bc3d14a35201c`: 30 ops
//! - `0xa6563baa758a7c39960a339750c89663aadf9fd2`: 17 ops
//! - block 94430259: 5 `arb_atomic` ops, max profit \$25
//! - blocks 94430973..=94431381: 8 liquidations by
//!   `0x7f1aacb852a7457e8eb97e5196276b92f408d990` (Aave V3)
//! - block 95893023: Aave V2 LiquidationCall on `0x8dFf…`, searcher
//!   `0xd9ff…76c3` (publicnode tip hunt; DefiLlama still shows Polygon V2 volume)
//!
//! BSC seeds (liq coverage plan Phase 3):
//! - block 127706714: Aave V3 LiquidationCall, searcher `0xec40d8aa…1118`
//! - block 102639252: Venus LiquidateBorrow (vUSDT), searcher `0x6898c2c1…e052`
//!
//! Avalanche seed (97505001..=98035036 record run; corpus DB, seeds in
//! `scenario_targets`):
//! - block 97783274: 1 `jit` (owner 0x3fed017e…, V3 tick range)
//! - block 97782322: 1 Aave V3 flash-loan `liquidation` (searcher 0xd9fffdf0…)
//! - block 97850836: 1 Benqi `liquidation` (Liquidator helper 0x5ac9bc…)
//! - block 97783552: 1 long-tail `arb_atomic` (searcher 0xf6d7c920…, Proximity v3)
//! - block 97781738: 1 claim_and_sell/airdrop (`unknown`, 0x04d9634d…)
//!
//! Ethereum seed (backfill 26055651..=26055745, 2026-09-25): 5 `sandwich`
//! ops. Three are from `0xae2fc483527b8ef99eb5d9b44875f005ba1fae13`, best
//! priced profit about \$10. No `jit` mint/burn pair landed in that window.
//!
//! Distant windows on one chain are ingested separately (see `ingest_windows`).
//!
//! Gated like every other live-RPC test: the live test is `#[ignore]`d, and
//! running it needs `MEV_SCOUT_E2E=1` + `RPC_URL` (`common::setup::rpc_url`).
//! Ignored, it does no network I/O:
//!
//! ```text
//! set MEV_SCOUT_E2E=1 & set RPC_URL=…
//! cargo test -p mev-scout-core --test explorer_corpus -- --ignored --nocapture
//! ```
//!
//! Once un-ignored, `MEV_SCOUT_E2E=1` without an `RPC_URL`, or an `RPC_URL`
//! with no reachable corpus chain, fails instead of passing silently.
//!
//! ## Seeding new cases (e.g. the missing `jit` kind)
//!
//! `MEV_SCOUT_RECORD=1` swaps the assertions for a report of the observed
//! per-kind / per-searcher / USD facts, printing every kind — a zero line is
//! how a rare kind is confirmed absent rather than merely unlisted. Without a
//! hunt window it re-reports the existing corpus windows.
//!
//! ```text
//! # re-record the current corpus windows
//! set MEV_SCOUT_E2E=1 & set RPC_URL=… & set MEV_SCOUT_RECORD=1
//! cargo test -p mev-scout-core --test explorer_corpus -- --nocapture
//!
//! # hunt a jit window outside the seeded ones. _CHAIN makes the run a hunt for
//! # that chain only (the other chains' corpus windows are not re-ingested);
//! # _FROM and _TO are both required and apply to that chain alone.
//! set MEV_SCOUT_RECORD_CHAIN=ethereum & set MEV_SCOUT_RECORD_FROM=… & set MEV_SCOUT_RECORD_TO=…
//! cargo test -p mev-scout-core --test explorer_corpus -- --nocapture
//! ```
//!
//! Then transcribe the report into a `CorpusCase`: take the `kind jit` count as
//! the `min_ops` floor and ~60-70% of `best profit` as `profit_usd_min` (the
//! same margin the seeds above use), and copy the top `searcher` verbatim.
//! `profit_usd` is only populated when a pool/price cache is available
//! (`MEV_SCOUT_POOL_CACHE`, else `cache/{chain}-mev-scout.sqlite`); without one
//! every USD field reads `$0.00` and `profit_usd_min` should stay `None`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;
use common::rpc_url;

use std::collections::HashMap;

use alloy::primitives::{address, Address};

use mev_scout_core::config::validation::resolve_chain;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::ingest::{run_range, IngestConfig, PoolViews, RangeRequest};
use mev_scout_core::explorer::scenario_targets::{
    hunt_targets, seeded_targets, RealSeedStatus, STRATEGY_SCENARIO_TARGETS,
};
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::explorer::MevKind;
use mev_scout_core::jobs::{init_rpc, load_pool_registry};
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::types::ChainName;

/// One golden real-block expectation.
struct CorpusCase {
    id: &'static str,
    chain: ChainName,
    from_block: u64,
    to_block: u64,
    kind: &'static str,
    min_ops: u64,
    searcher: Option<Address>,
    profit_usd_min: Option<f64>,
    /// Largest priced `profit_usd` in the window must sit at or under this.
    profit_usd_max: Option<f64>,
}

const CORPUS: &[CorpusCase] = &[
    CorpusCase {
        id: "av-arb-cluster",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 500,
        searcher: None,
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "av-arb-whale",
        chain: ChainName::Avalanche,
        from_block: 95681722,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 15,
        searcher: Some(address!("93a9f59d5defaae72702dedc2fc4a8fbc287a1ac")),
        profit_usd_min: Some(1000.0),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "av-arb-dust-searcher",
        chain: ChainName::Avalanche,
        from_block: 95681724,
        to_block: 95682322,
        kind: "arb_atomic",
        min_ops: 200,
        searcher: Some(address!("92d4ee32bc0f81dbb9923073395a2be7febbc3e5")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "av-arb-dense-block",
        chain: ChainName::Avalanche,
        from_block: 95682057,
        to_block: 95682057,
        kind: "arb_atomic",
        min_ops: 6,
        searcher: None,
        profit_usd_min: Some(100.0),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "av-liquidation-block",
        chain: ChainName::Avalanche,
        from_block: 95682033,
        to_block: 95682033,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("d2a82f1bb41a950ad24829b2f483b1b10f3569dd")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-arb-window",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 350,
        searcher: None,
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-arb-whale",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 6,
        searcher: Some(address!("8695330488513a6c3698a2b072ff88aaedbfac3e")),
        profit_usd_min: Some(20.0),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-arb-searcher",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 20,
        searcher: Some(address!("8c771167beba08d15acd9bee460bc3d14a35201c")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-arb-searcher-2",
        chain: ChainName::Polygon,
        from_block: 94_430_000,
        to_block: 94_430_280,
        kind: "arb_atomic",
        min_ops: 10,
        searcher: Some(address!("a6563baa758a7c39960a339750c89663aadf9fd2")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-arb-dense-block",
        chain: ChainName::Polygon,
        from_block: 94_430_259,
        to_block: 94_430_259,
        kind: "arb_atomic",
        min_ops: 3,
        searcher: None,
        profit_usd_min: Some(10.0),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "poly-liquidation-window",
        chain: ChainName::Polygon,
        from_block: 94_430_973,
        to_block: 94_431_381,
        kind: "liquidation",
        min_ops: 5,
        searcher: Some(address!("7f1aacb852a7457e8eb97e5196276b92f408d990")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "eth-sandwich-window",
        chain: ChainName::Ethereum,
        from_block: 26_055_651,
        to_block: 26_055_745,
        kind: "sandwich",
        min_ops: 3,
        searcher: None,
        profit_usd_min: Some(5.0),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "eth-sandwich-searcher",
        chain: ChainName::Ethereum,
        from_block: 26_055_651,
        to_block: 26_055_745,
        kind: "sandwich",
        min_ops: 2,
        searcher: Some(address!("ae2fc483527b8ef99eb5d9b44875f005ba1fae13")),
        profit_usd_min: Some(5.0),
        profit_usd_max: None,
    },
    // ETH backrun seeds from the 2026-10-25 re-hunt (drpc, 26264000..=26264962,
    // 963 blocks, 0 frontrun / 0 skim). Single-block causal pins; the explorer
    // re-ingests the block and re-runs the backrun annotation. A backrun is an
    // arb_atomic op carrying the `backrun` tag (see expected_tag). Count +
    // searcher only (no price cache in the E2E gate, so profit floors stay None).
    CorpusCase {
        id: "eth-backrun-26264539",
        chain: ChainName::Ethereum,
        from_block: 26_264_539,
        to_block: 26_264_539,
        kind: "arb_atomic",
        min_ops: 1,
        searcher: Some(address!("e85091501587b32a323be2512b36fab392d7c6bc")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "eth-backrun-26264858",
        chain: ChainName::Ethereum,
        from_block: 26_264_858,
        to_block: 26_264_858,
        kind: "arb_atomic",
        min_ops: 1,
        searcher: Some(address!("b52dae80b2ce64715d50ff3dfe4edd337f3d29bf")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Adjacent Polygon window (201 blocks) recorded without a pool cache: one
    // liquidation in block 94431566, from the same liquidator EOA as
    // `poly-liquidation-window` above. Priced at $0.00 (no position to bill),
    // so the case is count + searcher only.
    CorpusCase {
        id: "poly-liquidation-94431566",
        chain: ChainName::Polygon,
        from_block: 94_431_566,
        to_block: 94_431_566,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("7f1aacb852a7457e8eb97e5196276b92f408d990")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Polygon Aave V2 LiquidationCall on LendingPool 0x8dFf… (evidence-gated pin).
    CorpusCase {
        id: "poly-aave-v2-95893023",
        chain: ChainName::Polygon,
        from_block: 95_893_023,
        to_block: 95_893_023,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("d9fffdf0c4b18fb7e225da15b2af9f88e55676c3")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // BSC Aave V3 LiquidationCall on pool 0x6807… (aavescan BNB V3 seed).
    // Emitter remap leaves protocol=aave_v3 (no aave_v2_pool on BSC).
    CorpusCase {
        id: "bsc-aave-127706714",
        chain: ChainName::Bsc,
        from_block: 127_706_714,
        to_block: 127_706_714,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("ec40d8aa0f9493fa6d369347ab777e7709641118")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // BSC Venus LiquidateBorrow on vUSDT 0xfd58… via Venus Liquidator helper.
    // Alias map remaps emitter → protocol=venus; seize converted via c_token_underlyings.
    CorpusCase {
        id: "bsc-venus-102639252",
        chain: ChainName::Bsc,
        from_block: 102_639_252,
        to_block: 102_639_252,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("6898c2c1f07ed80c27c1370cd7c251a52246e052")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Second sandwich window, 84 blocks below the JIT round trip. Recorded with
    // MEV_SCOUT_RECORD=1 over 26059500..=26059550: 3 sandwich ops, best profit
    // $1.07 / net $0.19, spread over two searchers — the window case below
    // counts all three, the searcher case pins the second EOA on its own.
    CorpusCase {
        id: "eth-sandwich-26059500",
        chain: ChainName::Ethereum,
        from_block: 26_059_500,
        to_block: 26_059_550,
        kind: "sandwich",
        min_ops: 3,
        searcher: None,
        profit_usd_min: Some(0.5),
        profit_usd_max: None,
    },
    CorpusCase {
        id: "eth-sandwich-searcher-26059521",
        chain: ChainName::Ethereum,
        from_block: 26_059_521,
        to_block: 26_059_521,
        kind: "sandwich",
        min_ops: 1,
        searcher: Some(address!("196c00c1b00000000000007c00739ad9faa3ee77")),
        profit_usd_min: Some(0.5),
        profit_usd_max: None,
    },
    // Widest sandwich window of the corpus: 26059586..=26059700 recorded 9
    // sandwich ops at best profit $2.28, three of them from the recurring
    // 0xae2fc483… searcher. The floor is ~45% of the observed best rather than
    // the usual 60-70%: this window's best profit is one thin edge, so a
    // tighter band would turn a pricing regression into a flaky assertion.
    CorpusCase {
        id: "eth-sandwich-26059586",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_700,
        kind: "sandwich",
        min_ops: 9,
        searcher: None,
        profit_usd_min: Some(1.0),
        profit_usd_max: None,
    },
    // Real JIT round trip on UniswapV3 pool 0xd31d41df… (block 26059586):
    // owner 0x1f2f10d1… mints liquidity over ticks [-129060, -129000] and burns
    // the same liquidity later in the block, with an in-range swap at tick
    // -129014 in between, so the position was live for the swap and earned the
    // fee. Recorded with MEV_SCOUT_RECORD=1 over this single block.
    // `profit_usd_min` is unset because a jit op carries no realized profit:
    // it is a fee capture, priced from the pool's in-range swap volume.
    CorpusCase {
        id: "eth-jit-v3-round-trip",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_586,
        kind: "jit",
        min_ops: 1,
        searcher: Some(address!("1f2f10d1c40777ae1da742455c65828ff36df387")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Same window as the sandwich cases above: a jit mint/burn round trip in
    // block 26059518 from a different searcher, so the kind is pinned on two
    // distinct round trips rather than one. Priced at $0.00 in the record run
    // (the pool carried no in-range swap volume to bill), so no USD floor.
    CorpusCase {
        id: "eth-jit-26059518",
        chain: ChainName::Ethereum,
        from_block: 26_059_518,
        to_block: 26_059_518,
        kind: "jit",
        min_ops: 1,
        searcher: Some(address!("c36442b4a4522e871399cd717abdd847ab11fe88")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Two jit round trips in 26059586..=26059700 — the densest jit stretch
    // found, twice the rate of the 51-block window above. Asserted as a
    // window count because the record report only names the best tx's block;
    // the single op below pins one of the two to a block and an EOA.
    CorpusCase {
        id: "eth-jit-window-26059586",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_700,
        kind: "jit",
        min_ops: 2,
        searcher: None,
        profit_usd_min: None,
        profit_usd_max: None,
    },
    CorpusCase {
        id: "eth-jit-26059604",
        chain: ChainName::Ethereum,
        from_block: 26_059_604,
        to_block: 26_059_604,
        kind: "jit",
        min_ops: 1,
        searcher: Some(address!("410144dec85bb1929057829d54fb3b8d3f0ea4a3")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // A jit_arb (jit leg crossed with an arb leg) in block 26059522, the only
    // one the 26059500..=26059550 record run surfaced. Kept because the kind
    // is otherwise unsampled: jit_arb was in ALL_KINDS but had no case.
    CorpusCase {
        id: "eth-jit-arb-26059522",
        chain: ChainName::Ethereum,
        from_block: 26_059_522,
        to_block: 26_059_522,
        kind: "jit_arb",
        min_ops: 1,
        searcher: Some(address!("fc741c4258aaa097b281fecfb4ef9c012fa73db0")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Same block as the JIT round trip. Before this fix the three arb_atomic
    // ops on the block were priced at about $394_674_797_029_045. The ceiling
    // is the missing upper bound: real atomic arb on these pools is low
    // thousands of dollars, and the ops must still classify as arb_atomic.
    CorpusCase {
        id: "eth-arb-atomic-block-26059586",
        chain: ChainName::Ethereum,
        from_block: 26_059_586,
        to_block: 26_059_586,
        kind: "arb_atomic",
        min_ops: 3,
        searcher: None,
        profit_usd_min: None,
        profit_usd_max: Some(100_000.0),
    },
    // Avalanche baseline JIT: owner 0x3fed017e… mints a V3 position over ticks
    // [-253400, -253200] (opened 97782941) and burns it in block 97783274 with
    // an in-range swap in between. Realized fee capture — no realized profit
    // (fees estimated from in-range volume at 30 bps). Pinned seed for
    // `scenario_targets` baseline.jit.
    CorpusCase {
        id: "av-jit-97783274",
        chain: ChainName::Avalanche,
        from_block: 97_783_274,
        to_block: 97_783_274,
        kind: "jit",
        min_ops: 1,
        searcher: Some(address!("3fed017ec0f5517cdf2e8a9a4156c64d74252146")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Avalanche Aave V3 flash-loan liquidation in block 97782322: Aave
    // FlashLoan + LiquidationCall in the same tx (tags flash_loan_liq,
    // protocol aave_v3). Pinned seed for the P0.1 scenario.
    CorpusCase {
        id: "av-flash-loan-liq-97782322",
        chain: ChainName::Avalanche,
        from_block: 97_782_322,
        to_block: 97_782_322,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("d9fffdf0c4b18fb7e225da15b2af9f88e55676c3")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Avalanche Benqi LiquidateBorrow in block 97850836 from the Liquidator
    // helper (eoa == contract 0x5ac9bc…, non-indexed data layout, protocol
    // benqi). Pinned seed for the P0.2 scenario.
    CorpusCase {
        id: "av-benqi-97850836",
        chain: ChainName::Avalanche,
        from_block: 97_850_836,
        to_block: 97_850_836,
        kind: "liquidation",
        min_ops: 1,
        searcher: Some(address!("5ac9bc14376bf2727c6211960cf3cec1e2f295bd")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Avalanche long-tail atomic arb in block 97783552: profit token
    // 0xb97ef9ef… (non-blue-chip) routed through Proximity v3
    // (tags long_tail). Pinned seed for the P1.3 scenario.
    CorpusCase {
        id: "av-long-tail-97783552",
        chain: ChainName::Avalanche,
        from_block: 97_783_552,
        to_block: 97_783_552,
        kind: "arb_atomic",
        min_ops: 1,
        searcher: Some(address!("f6d7c920f6235abab9a9a5a6905ddbcaa1733394")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
    // Avalanche claim-and-sell (airdrop) in block 97781738: same-tx transfer
    // from 0x0 + swap selling the claimed token (tags claim_and_sell/airdrop,
    // kind unknown, USD approximate — MULTI_ASSET_PRICING). Pinned seed for the
    // P3.13 scenario.
    CorpusCase {
        id: "av-claim-sell-97781738",
        chain: ChainName::Avalanche,
        from_block: 97_781_738,
        to_block: 97_781_738,
        kind: "utility",
        min_ops: 1,
        searcher: Some(address!("04d9634df20aa66b23f114d37c45fa6c6ab76d56")),
        profit_usd_min: None,
        profit_usd_max: None,
    },
];

/// G-06: full 64-hex AVAX hashes pinned from
/// `docs/arb_atomic_audit_gaps_plan.md` §3. They are deliberately Rust
/// literals: the cache DB is git-ignored and must not be a test dependency.
/// `profit_usd`/`net_profit_usd` are the values recorded in that table; only
/// the first two rows have USD figures, so the other two assert the metadata
/// and a finite net only.
struct RealArbGolden {
    id: &'static str,
    block: u64,
    tx_hash: &'static str,
    arb_shape: &'static str,
    hop_count: u64,
    unique_tokens: u64,
    profit_usd: Option<f64>,
    net_profit_usd: Option<f64>,
    confidence: Option<&'static str>,
}

const REAL_ARB_GOLDEN: &[RealArbGolden] = &[
    RealArbGolden {
        id: "av-arb-100528634",
        block: 100_528_634,
        tx_hash: "0x37b413bbbd6af98b4919897223d39bfb3313f1e261232936c7b8cde54563bff9",
        arb_shape: "two_pool",
        hop_count: 3,
        unique_tokens: 2,
        profit_usd: Some(0.025902),
        net_profit_usd: Some(0.010206),
        confidence: Some("exact"),
    },
    RealArbGolden {
        id: "av-arb-100529098",
        block: 100_529_098,
        tx_hash: "0x7c709477088b85667b3b641d1b80546d833a6082b6570890244b975ecc106f1a",
        arb_shape: "multi_hop",
        hop_count: 4,
        unique_tokens: 4,
        profit_usd: Some(0.023446),
        net_profit_usd: Some(0.003250),
        confidence: Some("inferred"),
    },
    RealArbGolden {
        id: "av-arb-100529236",
        block: 100_529_236,
        tx_hash: "0x4d03e567171248c005a64da265667342bc1dedc6d9724e39643b0ba6268851e9",
        arb_shape: "multi_hop",
        hop_count: 4,
        unique_tokens: 4,
        profit_usd: None,
        net_profit_usd: None,
        confidence: Some("inferred"),
    },
    RealArbGolden {
        id: "av-arb-100529720",
        block: 100_529_720,
        tx_hash: "0x952f7e215908ea1d35fb59c43bc1c7dec109f935c18a615d62f0155025ea3d16",
        arb_shape: "triangular",
        hop_count: 4,
        unique_tokens: 3,
        profit_usd: None,
        net_profit_usd: None,
        confidence: None,
    },
];

/// USD figures in the plan were recorded from a priced cache, so an E2E run
/// without the same price cache can differ. This is a regression band, not an
/// exact pin; the exact recorded values stay in the plan table.
const GOLDEN_USD_REL_TOLERANCE: f64 = 0.5;

/// Build the runtime config for a corpus chain: defaults + chain + the
/// caller's RPC. If the repo pool-registry cache exists it is wired in so
/// classification matches the seed run; otherwise the registry degrades to
/// transfer-pairing (best-effort, same as production without a cache).
#[allow(clippy::field_reassign_with_default)]
fn corpus_config(chain: ChainName, rpc_url: &str) -> Config {
    let mut config = Config::default();
    config.chain = chain;
    config.rpc.rpc_url = Some(rpc_url.to_string());

    let cache_path = match std::env::var("MEV_SCOUT_POOL_CACHE") {
        Ok(p) if !p.is_empty() => Some(p),
        _ => {
            let fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("cache")
                .join(format!("{chain}-mev-scout.sqlite"));
            fallback
                .exists()
                .then(|| fallback.to_string_lossy().into_owned())
        }
    };
    if let Some(p) = cache_path {
        config.output.db_path = p;
    }
    config
}

/// Assert one corpus case against an already-ingested store.
fn assert_case(store: &ExplorerStore, case: &CorpusCase) {
    let kind = MevKind::parse(case.kind)
        .unwrap_or_else(|| panic!("{}: invalid kind '{}'", case.id, case.kind));
    let mut ops = store
        .ops_in_range(case.from_block, case.to_block, &[kind], None)
        .unwrap_or_else(|e| panic!("{}: ops_in_range query failed: {e}", case.id));

    if let Some(searcher) = case.searcher {
        let target = format!("{searcher:#x}").to_ascii_lowercase();
        ops.retain(|o| o.eoa.to_ascii_lowercase() == target);
    }

    let observed = ops.len() as u64;
    assert!(
        observed >= case.min_ops,
        "{}: expected >= {} {} ops in {}..={}, observed {}",
        case.id,
        case.min_ops,
        case.kind,
        case.from_block,
        case.to_block,
        observed
    );

    if let Some(floor) = case.profit_usd_min {
        let best = ops
            .iter()
            .filter_map(|o| o.profit_usd)
            .fold(0.0f64, f64::max);
        assert!(
            best >= floor,
            "{}: expected best {} profit >= ${floor}, observed ${best}",
            case.id,
            case.kind
        );
    }

    if let Some(cap) = case.profit_usd_max {
        let best = ops
            .iter()
            .filter_map(|o| o.profit_usd)
            .fold(0.0f64, f64::max);
        assert!(
            best.is_finite() && best <= cap,
            "{}: expected best {} profit <= ${cap}, observed ${best}",
            case.id,
            case.kind
        );
    }

    if let Some(want) = expected_protocol(case.id) {
        let hit = ops.iter().any(|o| {
            o.details_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("protocol")
                        .and_then(|p| p.as_str())
                        .map(|p| p == want)
                })
                .unwrap_or(false)
        });
        assert!(
            hit,
            "{}: expected details.protocol == {want:?} on at least one op",
            case.id
        );
    }

    if let Some(want) = expected_tag(case.id) {
        let hit = ops.iter().any(|o| {
            o.details_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("tags")
                        .and_then(|t| t.as_array())
                        .map(|a| a.iter().any(|x| x.as_str() == Some(want)))
                })
                .unwrap_or(false)
        });
        assert!(
            hit,
            "{}: expected details.tags to contain {want:?} on at least one op",
            case.id
        );
    }

    eprintln!(
        "{}: ok — {} {} ops in {}..={} (floor {})",
        case.id, observed, case.kind, case.from_block, case.to_block, case.min_ops
    );
}

/// Tag expectations for corpus cases whose kind alone is not the assertion
/// (a backrun is an `arb_atomic` op carrying the `backrun` tag).
fn expected_tag(case_id: &str) -> Option<&'static str> {
    match case_id {
        id if id.starts_with("eth-backrun-") => Some("backrun"),
        _ => None,
    }
}

/// Protocol-label expectations for liquidation corpus cases (plan verify).
fn expected_protocol(case_id: &str) -> Option<&'static str> {
    match case_id {
        "poly-liquidation-window" | "poly-liquidation-94431566" => Some("aave_v3"),
        id if id.starts_with("poly-aave-v2-") => Some("aave_v2"),
        id if id.starts_with("bsc-aave-") => Some("aave_v3"),
        id if id.starts_with("bsc-venus-") => Some("venus"),
        "av-flash-loan-liq-97782322" => Some("aave_v3"),
        "av-benqi-97850836" => Some("benqi"),
        _ => None,
    }
}

/// Merge case ranges that sit within `GAP` blocks of each other. Distant
/// windows on the same chain are ingested on their own so the harness does
/// not replay every block between them.
fn ingest_windows(cases: &[&CorpusCase]) -> Vec<(u64, u64)> {
    const GAP: u64 = 64;
    let mut ranges: Vec<(u64, u64)> = cases.iter().map(|c| (c.from_block, c.to_block)).collect();
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (from, to) in ranges {
        if let Some(last) = out.last_mut() {
            if from <= last.1.saturating_add(GAP) {
                last.1 = last.1.max(to);
                continue;
            }
        }
        out.push((from, to));
    }
    out
}

/// Ingest each cluster of corpus cases for one chain, then assert every case.
/// Skips (never fails) on RPC/network problems so a flaky endpoint does not
/// turn the corpus red — assertion failures are the only hard failures.
async fn run_chain_corpus(chain: ChainName, rpc_url: &str) -> bool {
    let cases: Vec<&CorpusCase> = CORPUS.iter().filter(|c| c.chain == chain).collect();
    // A hunt window applies only to the chain it names, so an Ethereum range is
    // never replayed against the Avalanche/Polygon corpus windows.
    let windows = record_windows(chain).unwrap_or_else(|| ingest_windows(&cases));
    if windows.is_empty() {
        return true;
    }
    let union_to = windows.iter().map(|(_, to)| *to).max().unwrap();

    let config = corpus_config(chain, rpc_url);
    let setup = match init_rpc(&config, chain, true).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: RPC init failed for {chain}: {e}");
            return false;
        }
    };
    let tip = match setup.rpc.get_block_number().await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Skipping: get_block_number failed for {chain}: {e}");
            return false;
        }
    };
    if tip < union_to {
        eprintln!(
            "Skipping: RPC tip {tip} is below corpus window {union_to} — \
             wrong chain, or the endpoint is far behind"
        );
        return false;
    }

    let (_, chain_cfg) = match resolve_chain(&config) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Skipping: resolve_chain failed for {chain}: {e}");
            return false;
        }
    };
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;

    let registry = load_pool_registry(&config, &chain);
    let store = match ExplorerStore::open_in_memory() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: in-memory store open failed: {e}");
            return false;
        }
    };

    for (from, to) in &windows {
        eprintln!(
            "{chain}: ingesting corpus window {from}..={to} (~{} blocks)",
            to - from + 1
        );
        if let Err(e) = run_range(
            &setup.rpc,
            &store,
            &cfg,
            PoolViews {
                tokens: &registry.tokens,
                v2_like: &registry.v2_like,
                epoch_venue: &registry.epoch_venue,
                fee_bps: &registry.fee_bps,
                dex_name: &registry.dex_name,
            },
            RangeRequest {
                from_block: *from,
                to_block: *to,
                block_concurrency: 1,
            },
            &NoopProgress,
        )
        .await
        {
            eprintln!("Skipping: corpus ingest failed for {chain} {from}..={to}: {e}");
            return false;
        }
    }

    if recording() {
        record_report(&store, &windows);
    } else {
        for case in cases {
            assert_case(&store, case);
        }
    }
    true
}

/// Every kind the classifier can emit, mirroring `MevKind::ALL`. A record run
/// reports all of them — including the ones with a zero count, which is how an
/// absent rare kind (`jit`) is confirmed absent rather than merely unlisted.
/// Retired labels (`sandwich`, `frontrun`, `jit_arb`, `backrun`) are not
/// emittable; a backrun is now an `arb_atomic` row carrying the `backrun` tag.
const ALL_KINDS: &[&str] = &[
    "arb_atomic",
    "jit",
    "liquidation",
    "liquidity_add",
    "liquidity_remove",
    "reward_claim",
    "skim",
    "utility",
];

/// `MEV_SCOUT_RECORD=1` — report derived facts instead of asserting them.
fn recording() -> bool {
    std::env::var("MEV_SCOUT_RECORD").is_ok_and(|v| v == "1")
}

/// The hunt window (`MEV_SCOUT_RECORD_FROM` + `_TO`) for `chain`. Both bounds
/// are required, and the window applies *only* to a chain named by
/// `MEV_SCOUT_RECORD_CHAIN` — otherwise the corpus windows of that chain are
/// used, so a range is never replayed against the wrong chain's blocks.
fn record_windows(chain: ChainName) -> Option<Vec<(u64, u64)>> {
    if !record_chains().contains(&chain) {
        return None;
    }
    let from = std::env::var("MEV_SCOUT_RECORD_FROM").ok()?;
    let to = std::env::var("MEV_SCOUT_RECORD_TO").ok()?;
    parse_hunt_window(&from, &to)
}

/// Parse the hunt window; `None` on a non-numeric or inverted range, which
/// leaves the corpus windows in place.
fn parse_hunt_window(from: &str, to: &str) -> Option<Vec<(u64, u64)>> {
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = to.trim().parse().ok()?;
    (from <= to).then(|| vec![(from, to)])
}

/// Extra chains named by `MEV_SCOUT_RECORD_CHAIN` (comma-separated), so a hunt
/// can run on a chain the corpus does not cover yet.
fn record_chains() -> Vec<ChainName> {
    let Ok(raw) = std::env::var("MEV_SCOUT_RECORD_CHAIN") else {
        return Vec::new();
    };
    parse_chain_list(&raw)
}

/// Unknown names are skipped rather than aborting the hunt.
fn parse_chain_list(raw: &str) -> Vec<ChainName> {
    raw.split(',')
        .filter_map(|c| c.trim().parse::<ChainName>().ok())
        .collect()
}

/// Print observed per-kind / per-searcher / USD facts for the ingested
/// windows. Everything a `CorpusCase` asserts is visible here, so a recorded
/// run can be transcribed into a case field-for-field.
fn record_report(store: &ExplorerStore, windows: &[(u64, u64)]) {
    let mut lines = Vec::new();
    lines.push("--- strategy scenario hunt checklist ---".into());
    lines.push(format!(
        "catalogue {} rows ({} seeded, {} hunt, {} synthetic-only)",
        STRATEGY_SCENARIO_TARGETS.len(),
        seeded_targets().count(),
        hunt_targets().count(),
        STRATEGY_SCENARIO_TARGETS
            .iter()
            .filter(|t| t.real == RealSeedStatus::SyntheticOnly)
            .count(),
    ));
    for t in hunt_targets() {
        lines.push(format!("  HUNT {} [{}]: {}", t.id, t.label, t.hunt));
    }
    for (from, to) in windows {
        lines.push(format!("{from}..={to}"));
        let ops = match store.ops_in_range(*from, *to, &[], None) {
            Ok(ops) => ops,
            Err(e) => {
                lines.push(format!("  ops query failed: {e}"));
                continue;
            }
        };
        for kind in ALL_KINDS {
            let of_kind: Vec<_> = ops.iter().filter(|o| o.kind == *kind).collect();
            if of_kind.is_empty() {
                lines.push(format!("  kind {kind}: 0 ops"));
                continue;
            }
            let mut searchers: HashMap<&str, usize> = HashMap::new();
            let mut best_profit = 0.0f64;
            let mut best_net = 0.0f64;
            for op in &of_kind {
                *searchers.entry(op.eoa.as_str()).or_default() += 1;
                best_profit = best_profit.max(op.profit_usd.unwrap_or(0.0));
                best_net = best_net.max(op.net_profit_usd.unwrap_or(0.0));
            }
            lines.push(format!(
                "  kind {kind}: {} ops, best profit ${best_profit:.2}, best net ${best_net:.2}",
                of_kind.len()
            ));
            let mut top: Vec<_> = searchers.into_iter().collect();
            top.sort_by_key(|b| std::cmp::Reverse(b.1));
            for (searcher, n) in top.into_iter().take(3) {
                lines.push(format!("    searcher {searcher}: {n}"));
            }
            if let Some(best) = of_kind.iter().max_by(|a, b| {
                a.profit_usd
                    .unwrap_or(0.0)
                    .total_cmp(&b.profit_usd.unwrap_or(0.0))
            }) {
                lines.push(format!(
                    "    best tx {} in block {} (eoa {})",
                    best.tx_hash, best.block_number, best.eoa
                ));
            }
        }
    }
    println!("{}", lines.join("\n"));
}

/// Shared live gate: `None` means "not requested", an error means an
/// explicitly requested run is misconfigured and must not read as a pass.
fn live_rpc_url() -> anyhow::Result<Option<String>> {
    let Some(url) = rpc_url() else {
        if std::env::var("MEV_SCOUT_E2E").as_deref() == Ok("1") {
            anyhow::bail!("MEV_SCOUT_E2E=1 but RPC_URL is unset or empty");
        }
        if std::env::var("RPC_URL").is_ok_and(|v| !v.is_empty()) {
            anyhow::bail!(
                "RPC_URL is set but MEV_SCOUT_E2E=1 is not — set MEV_SCOUT_E2E=1 to run \
                 this live test, or unset RPC_URL to skip it"
            );
        }
        return Ok(None);
    };
    Ok(Some(url))
}

#[tokio::test]
#[ignore = "live RPC — set MEV_SCOUT_E2E=1 and RPC_URL"]
async fn real_block_corpus_matches_derived_facts() -> anyhow::Result<()> {
    let Some(rpc_url) = live_rpc_url()? else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return Ok(());
    };

    // A hunt (`MEV_SCOUT_RECORD_CHAIN`) is about the named chain only, so it
    // does not also re-ingest every other chain's corpus windows. Without
    // `_CHAIN` the run re-records the whole corpus.
    let hunt = record_chains();
    let mut chains: Vec<ChainName> = Vec::new();
    if hunt.is_empty() {
        for case in CORPUS {
            if !chains.contains(&case.chain) {
                chains.push(case.chain);
            }
        }
    } else {
        chains = hunt.clone();
    }

    let mut checked = 0usize;
    for chain in &chains {
        if run_chain_corpus(*chain, &rpc_url).await {
            checked += 1;
        }
    }
    if checked == 0 {
        anyhow::bail!("RPC_URL supplied but no corpus chain was reachable");
    }
    // A named chain with no cases and no usable hunt window has nothing to
    // ingest, so the record run would exit silently — say so.
    if recording() {
        for chain in &hunt {
            if record_windows(*chain).is_none() && !CORPUS.iter().any(|case| case.chain == *chain) {
                eprintln!(
                    "Record: {chain} has no corpus cases and no MEV_SCOUT_RECORD_FROM/_TO \
                     window — nothing to report"
                );
            }
        }
    }
    Ok(())
}

/// Assert one pinned real hash against its persisted row. The `arb_shape`
/// family and the route are exact; USD values get a documented band because
/// the recorded figures came from a priced cache that an E2E run may not have.
fn assert_real_arb_golden(store: &ExplorerStore, case: &RealArbGolden) {
    let mut ops = store
        .ops_for_tx(case.tx_hash)
        .unwrap_or_else(|e| panic!("{}: ops_for_tx failed: {e}", case.id));
    ops.retain(|o| o.kind == "arb_atomic");
    let op = ops
        .first()
        .unwrap_or_else(|| panic!("{}: no arb_atomic op for {}", case.id, case.tx_hash));

    assert_eq!(op.block_number, case.block, "{}: block mismatch", case.id);
    if let Some(want) = case.confidence {
        assert_eq!(op.confidence, want, "{}: confidence mismatch", case.id);
    }

    let details: serde_json::Value = serde_json::from_str(op.details_json.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|e| panic!("{}: details_json parse failed: {e}", case.id));
    let meta = &details["arb_meta"];
    assert_eq!(
        meta["arb_shape"].as_str(),
        Some(case.arb_shape),
        "{}: arb_shape mismatch",
        case.id
    );
    assert_eq!(
        meta["arb_shape_basis"].as_str(),
        Some("token_count"),
        "{}: arb_shape_basis mismatch",
        case.id
    );
    assert_eq!(
        meta["hop_count"].as_u64(),
        Some(case.hop_count),
        "{}: hop_count mismatch",
        case.id
    );
    assert_eq!(
        meta["unique_tokens"].as_u64(),
        Some(case.unique_tokens),
        "{}: unique_tokens mismatch",
        case.id
    );

    // Route: both the separate persisted column and details["route"] must be
    // present and agree with the hop count.
    let route_in_details = details["route"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: details.route is not an array", case.id));
    assert_eq!(
        route_in_details.len() as u64,
        case.hop_count,
        "{}: route edge count mismatch",
        case.id
    );
    let route_json = op
        .route_json
        .as_deref()
        .unwrap_or_else(|| panic!("{}: route_json missing", case.id));
    let route_column: Vec<serde_json::Value> = serde_json::from_str(route_json)
        .unwrap_or_else(|e| panic!("{}: route_json parse failed: {e}", case.id));
    assert_eq!(
        route_column.len(),
        route_in_details.len(),
        "{}: route_json and details.route disagree",
        case.id
    );

    // cross_dex_basis: protocol identity is only claimed when every leg has a
    // pool_dex; otherwise the AMM-family fallback is used. Assert the observed
    // basis is one of the two and internally consistent with cross-dex counts.
    let basis = meta["cross_dex_basis"]
        .as_str()
        .unwrap_or_else(|| panic!("{}: cross_dex_basis missing", case.id));
    assert!(
        matches!(basis, "protocol" | "amm_family"),
        "{}: unknown cross_dex_basis {basis:?}",
        case.id
    );
    let cross = meta["is_cross_dex"]
        .as_bool()
        .unwrap_or_else(|| panic!("{}: is_cross_dex missing", case.id));
    let dex_count = meta["dex_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("{}: dex_count missing", case.id));
    assert_eq!(
        cross,
        dex_count > 1,
        "{}: is_cross_dex disagrees with dex_count",
        case.id
    );
    if basis == "protocol" {
        let venues = meta["venues"]
            .as_array()
            .unwrap_or_else(|| panic!("{}: protocol basis without venues", case.id));
        assert!(!venues.is_empty(), "{}: empty protocol venues", case.id);
    } else {
        assert!(
            meta.get("venues").is_none(),
            "{}: amm_family basis must not publish protocol venues",
            case.id
        );
    }

    // P&L: always finite, with a regression band where the plan records a
    // value. `net_profit_usd` is the canonical net; no recomputation here.
    let net = op
        .net_profit_usd
        .unwrap_or_else(|| panic!("{}: net_profit_usd missing", case.id));
    assert!(net.is_finite(), "{}: non-finite net_profit_usd", case.id);
    if let Some(expected) = case.net_profit_usd {
        assert_usd_band(net, expected, case.id, "net_profit_usd");
    }
    if let Some(expected) = case.profit_usd {
        let profit = op
            .profit_usd
            .unwrap_or_else(|| panic!("{}: profit_usd missing", case.id));
        assert_usd_band(profit, expected, case.id, "profit_usd");
    }

    eprintln!(
        "{}: ok — shape={} basis={basis} hops={} net=${net:.6}",
        case.id, case.arb_shape, case.hop_count
    );
}

fn assert_usd_band(actual: f64, expected: f64, id: &str, field: &str) {
    let low = expected * (1.0 - GOLDEN_USD_REL_TOLERANCE);
    let high = expected * (1.0 + GOLDEN_USD_REL_TOLERANCE);
    assert!(
        actual.is_finite() && (low..=high).contains(&actual),
        "{id}: expected {field} in [${low:.6}, ${high:.6}] \
         (recorded ${expected:.6}), observed ${actual:.6}"
    );
}

/// G-06 acceptance: replay four full AVAX hashes from an archive RPC and
/// reproduce route, `arb_meta.arb_shape`, `cross_dex_basis` and canonical net.
#[tokio::test]
#[ignore = "live RPC — set MEV_SCOUT_E2E=1 and RPC_URL (archive for AVAX 100.5M)"]
async fn pinned_real_arb_hashes_reproduce_route_meta_and_pnl() -> anyhow::Result<()> {
    let Some(rpc_url) = live_rpc_url()? else {
        eprintln!("Skipping: MEV_SCOUT_E2E/RPC_URL not set");
        return Ok(());
    };

    let chain = ChainName::Avalanche;
    let config = corpus_config(chain, &rpc_url);
    let setup = init_rpc(&config, chain, true).await?;
    let tip = setup.rpc.get_block_number().await?;
    let golden_tip = REAL_ARB_GOLDEN
        .iter()
        .map(|c| c.block)
        .max()
        .unwrap_or_default();
    anyhow::ensure!(
        tip >= golden_tip,
        "RPC tip {tip} is below pinned golden block {golden_tip}; an archive endpoint is required"
    );

    let (_, chain_cfg) = resolve_chain(&config)?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    let registry = load_pool_registry(&config, &chain);
    let store = ExplorerStore::open_in_memory()?;

    for case in REAL_ARB_GOLDEN {
        run_range(
            &setup.rpc,
            &store,
            &cfg,
            PoolViews {
                tokens: &registry.tokens,
                v2_like: &registry.v2_like,
                epoch_venue: &registry.epoch_venue,
                fee_bps: &registry.fee_bps,
                dex_name: &registry.dex_name,
            },
            RangeRequest {
                from_block: case.block,
                to_block: case.block,
                block_concurrency: 1,
            },
            &NoopProgress,
        )
        .await?;
        assert_real_arb_golden(&store, case);
    }
    Ok(())
}

#[test]
fn hunt_window_parses_and_rejects_bad_input() {
    assert_eq!(parse_hunt_window(" 100 ", "200"), Some(vec![(100, 200)]));
    assert_eq!(parse_hunt_window("200", "200"), Some(vec![(200, 200)]));
    assert_eq!(parse_hunt_window("200", "100"), None, "inverted range");
    assert_eq!(parse_hunt_window("abc", "200"), None, "non-numeric");
    assert_eq!(parse_hunt_window("", "200"), None, "missing bound");
}

#[test]
fn hunt_chain_list_skips_unknown_names() {
    assert_eq!(
        parse_chain_list("ethereum, polygon"),
        vec![ChainName::Ethereum, ChainName::Polygon]
    );
    assert_eq!(
        parse_chain_list(" ethereum , , base "),
        vec![ChainName::Ethereum, ChainName::Base]
    );
    assert!(parse_chain_list("notachain").is_empty());
    assert!(parse_chain_list("").is_empty());
}

#[test]
fn every_classifier_kind_is_reported_by_a_record_run() {
    // A kind absent from ALL_KINDS would print no line at all, so a rare kind
    // like `jit` could never be confirmed absent. Both sides derive from the
    // enum, so a new variant fails until the corpus list follows it.
    let expected: Vec<&str> = MevKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(ALL_KINDS, expected, "ALL_KINDS must mirror MevKind::ALL");
}

#[test]
fn strategy_scenario_targets_match_avalanche_corpus_seeds() {
    // Seeded catalogue rows must sit inside an Avalanche CORPUS window of the
    // same kind (or a kind-compatible label such as arb_atomic / liquidation).
    for t in seeded_targets() {
        let from = t.seed_from.expect("seeded");
        let to = t.seed_to.expect("seeded");
        let kind = match t.label {
            "arb_atomic" | "long_tail" | "flash_arb" | "backrun" => "arb_atomic",
            "liquidation" | "benqi" | "flash_loan_liq" => "liquidation",
            "claim_and_sell" => "utility",
            other => other,
        };
        let hit = CORPUS.iter().any(|c| {
            c.chain == ChainName::Avalanche
                && c.kind == kind
                && c.from_block <= from
                && c.to_block >= to
        });
        assert!(
            hit,
            "seeded target {} [{}] {}..={} has no Avalanche CORPUS cover",
            t.id, t.label, from, to
        );
    }
}
