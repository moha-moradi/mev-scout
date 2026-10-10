//! Backrun detector unit + integration tests â€” `docs/plan_backrun.md` آ§6.1.
//!
//! Pins the D3 net-flip rule (`net(pre) <= 0 && net(post) > 0`), the scope
//! gates (`will_touch` / `newly_dirty`), cross-family dedup, D2 precedence
//! (`suppress_superseded_arbs`) and the backrun canonical-id form.

use std::collections::HashSet;

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use mev_scout_core::data::{ExecutedLog, TxData};
use mev_scout_core::mev::detectors::backrun::{
    suppress_superseded_arbs, BackrunDetector, BackrunKey,
};
use mev_scout_core::mev::detectors::multi_hop::MultiHopArbDetector;
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::mev::detectors::DetectCtx;
use mev_scout_core::pool::state::{PoolManager, ScanScope};
use mev_scout_core::types::{GasConfig, GasModel, MevOpportunity, Strategy};

mod common;
use common::*;

const BLOCK: u64 = 1_000_000;
const TS: u64 = 12_345_678;
const BASE_FEE: u128 = 50_000_000_000;
/// Anchor (victim) transaction index used across the detector-level tests.
const VICTIM: usize = 1;

/// Uniswap V2 `Swap(address,uint256,uint256,uint256,uint256,address)` topic0
/// (sender and `to` are indexed; the 4 amounts are the data payload).
/// Must match `chain::events::V2_SWAP_TOPIC` — see `explorer/decode.rs`.
fn v2_swap_topic() -> B256 {
    keccak256("Swap(address,uint256,uint256,uint256,uint256,address)")
}

/// V2 Swap log, data layout `(amt0_in, amt1_in, amt0_out, amt1_out)`.
fn v2_swap(
    pool: Address,
    amt0_in: u128,
    amt1_in: u128,
    amt0_out: u128,
    amt1_out: u128,
) -> ExecutedLog {
    let word = |v: u128| {
        let mut b = vec![0u8; 32];
        b[16..].copy_from_slice(&v.to_be_bytes());
        Bytes::from(b)
    };
    ExecutedLog {
        address: pool,
        topics: vec![v2_swap_topic(), B256::ZERO, B256::ZERO],
        data: [word(amt0_in), word(amt1_in), word(amt0_out), word(amt1_out)]
            .concat()
            .into(),
    }
}

fn txs(n: usize) -> Vec<TxData> {
    (0..n)
        .map(|i| TxData {
            hash: B256::repeat_byte(i as u8 + 1),
            index: i as u64,
            tx_type: 2,
            from: Address::repeat_byte(0x10 + i as u8),
            to: Some(Address::repeat_byte(0x42)),
            input: Bytes::new(),
            value: U256::ZERO,
            gas_limit: 100_000,
            max_fee_per_gas: BASE_FEE,
            max_priority_fee_per_gas: None,
            gas_price: None,
            nonce: i as u64,
            access_list: vec![],
            authorization_list: vec![],
        })
        .collect()
}

/// Two V2 pools at identical prices â€” no arb is detectable (D3 `net(pre) <= 0`).
fn balanced_pair() -> PoolManager {
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000_000_000,
        2_000_000_000_000_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000_000_000,
        2_000_000_000_000_000_000,
    ));
    pm.with_wrapped_native(wmatic())
}

fn pair_touch() -> HashSet<Address> {
    [matic_usdc_pool(), matic_usdt_pool()].into()
}

/// Victim swap: buy 1e18 WMATIC out of the USDT pool, doubling its USDT side.
/// Post-state price ratio is about 4x â€” the same shape as
/// `synthetic_arb_pools()`, which the replay suite proves is net-positive
/// after gas.
fn victim_skew() -> ExecutedLog {
    v2_swap(
        matic_usdt_pool(),
        1_020_000_000_000,
        0,
        0,
        1_000_000_000_000_000_000,
    )
}

fn ctx<'a>(pm: &'a PoolManager, tx: usize, scope: &'a ScanScope<'_>) -> DetectCtx<'a> {
    DetectCtx::new(pm, tx, TS, BASE_FEE, GasConfig::default(), scope)
}

fn pre(
    d: &mut BackrunDetector,
    pm: &PoolManager,
    tx: usize,
    scope: &ScanScope,
) -> Vec<MevOpportunity> {
    d.pre_detect(ctx(pm, tx, scope))
}

fn post(
    d: &mut BackrunDetector,
    pm: &PoolManager,
    tx: usize,
    scope: &ScanScope,
    txs: &[TxData],
) -> Vec<MevOpportunity> {
    d.post_detect(ctx(pm, tx, scope), &[], txs)
}

fn probe_two(pm: &PoolManager, scope: &ScanScope) -> Vec<MevOpportunity> {
    let mut d = TwoHopArbDetector::new(BLOCK);
    d.detect(ctx(pm, VICTIM, scope))
}

fn probe_multi(pm: &PoolManager, scope: &ScanScope) -> Vec<MevOpportunity> {
    let mut d = MultiHopArbDetector::new(BLOCK);
    d.detect(ctx(pm, VICTIM, scope))
}

/// Victim flips an unexecutable gap into an executable one.
#[test]
fn flips_from_unprofitable_to_profitable() {
    let mut pm = balanced_pair();
    let touch = pair_touch();
    let txs = txs(3);
    let mut d = BackrunDetector::new(BLOCK);

    let pre_opps = pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch));
    assert!(
        pre_opps.is_empty(),
        "balanced pools must not pre-detect: {pre_opps:?}"
    );

    pm.update_from_logs(&[victim_skew()]);
    let backruns = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);

    assert_eq!(backruns.len(), 1, "exactly one backrun claim: {backruns:?}");
    let o = &backruns[0];
    assert_eq!(o.strategy, Strategy::Backrun);
    assert_eq!(o.victim_tx_index, Some(VICTIM));
    assert_eq!(o.backrun_tx_index, None);
    assert_eq!(
        o.tx_index, VICTIM,
        "tx_index stays the mechanical anchor index"
    );
    assert!(
        o.expected_profit > U256::from(o.gas_cost_wei),
        "post-image must be net-positive: profit={} gas={}",
        o.expected_profit,
        o.gas_cost_wei
    );
    assert_eq!(
        o.sender,
        Some(txs[VICTIM].from),
        "sender is the anchor (victim) sender"
    );
    assert_eq!(
        o.tx_hash,
        Some(txs[VICTIM].hash),
        "tx_hash anchors the victim"
    );
}

/// D3 regression: a gap that predates the victim is a plain arb, not a
/// backrun (`net(pre) > 0` fails the flip).
#[test]
fn no_claim_when_gap_predates_victim() {
    let mut pm = synthetic_arb_pools();
    let touch: HashSet<Address> = [
        alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        alloy::primitives::address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    ]
    .into();
    let txs = txs(3);
    let mut d = BackrunDetector::new(BLOCK);

    let pre_opps = pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch));
    assert!(!pre_opps.is_empty(), "fixture must pre-detect the open gap");
    assert!(
        pre_opps
            .iter()
            .any(|o| o.expected_profit > U256::from(o.gas_cost_wei)),
        "the pre-existing gap must already be executable (net(pre) > 0)"
    );

    // Victim perturbs one fixture pool but leaves the gap open.
    pm.update_from_logs(&[v2_swap(
        alloy::primitives::address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        1_000_000_000,
        0,
        0,
        100_000_000_000_000,
    )]);

    // Sanity: the post-image opportunity still exists, else the empty result
    // below would pass for the wrong reason.
    let probe = probe_two(&pm, &ScanScope::Dirty(&touch));
    assert!(
        !probe.is_empty(),
        "gap must still be detectable after the victim"
    );
    assert!(
        probe
            .iter()
            .any(|o| o.expected_profit > U256::from(o.gas_cost_wei)),
        "gap must still be net-positive after the victim"
    );

    let backruns = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);
    assert!(
        backruns.is_empty(),
        "pre-existing gap must not be claimed as a backrun: {backruns:?}"
    );
}

/// A victim that dirties no tracked pool never reaches the post pass.
/// The synthetic fixture's receipt logs are empty (`common/setup.rs`), so
/// `will_touch` and `newly_dirty` are both empty in `run_block`.
#[tokio::test]
async fn no_claim_when_state_unchanged() {
    let dir = temp_test_dir("backrun_unchanged");
    let gas_cfg = GasConfig {
        gas_model: GasModel::HistoricalExact,
        ..GasConfig::default()
    };
    let mut runner = make_synthetic_runner(&dir, 1, gas_cfg);
    let (opps, stats, _) = runner.run_block(1).unwrap();
    assert_eq!(stats.total_tx_count, 2, "synthetic block carries 2 txs");

    assert!(
        !opps.iter().any(|o| o.strategy == Strategy::Backrun),
        "no logs means no state change means no backrun claim"
    );
    assert!(
        opps.iter()
            .any(|o| matches!(o.strategy, Strategy::TwoHopArb | Strategy::MultiHopArb)),
        "plain arbs from the fixture must still be emitted"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `A` moves the pool but `net(post) <= 0`: no claim.
#[test]
fn no_claim_when_still_unprofitable_after() {
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        1_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_001_000,
        1_000_000,
    ));
    let mut pm = pm.with_wrapped_native(wmatic());
    let touch = pair_touch();
    let txs = txs(3);
    let mut d = BackrunDetector::new(BLOCK);

    let pre_opps = pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch));
    assert!(pre_opps.is_empty(), "balanced fixture must not pre-detect");

    // Victim opens a sub-gas gap: detectable as an opportunity, never
    // executable after gas.
    pm.update_from_logs(&[v2_swap(matic_usdt_pool(), 10_100, 0, 0, 10_000)]);

    let probe = probe_two(&pm, &ScanScope::Dirty(&touch));
    assert!(!probe.is_empty(), "sanity: the tiny gap must be detectable");
    assert!(
        probe
            .iter()
            .all(|o| o.expected_profit <= U256::from(o.gas_cost_wei)),
        "sanity: the tiny gap must be below gas"
    );

    let backruns = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);
    assert!(
        backruns.is_empty(),
        "net(post) <= 0 must not be claimed: {backruns:?}"
    );
}

/// The victim's `will_touch` scope excludes an untouched path, so the post
/// pass cannot see it even though its gap is open.
#[test]
fn no_claim_when_path_never_touched_by_victim() {
    let mut pm = synthetic_arb_pools();
    // A second, independent pair (fresh token universe) the victim touches.
    let tok_x = alloy::primitives::address!("1000000000000000000000000000000000000001");
    let tok_y = alloy::primitives::address!("2000000000000000000000000000000000000002");
    let tok_z = alloy::primitives::address!("3000000000000000000000000000000000000003");
    let c = alloy::primitives::address!("cccccccccccccccccccccccccccccccccccccccc");
    let d_pool = alloy::primitives::address!("dddddddddddddddddddddddddddddddddddddddd");
    pm.add_pool(make_pool(c, tok_x, tok_y, 1_000_000, 1_000_000));
    pm.add_pool(make_pool(d_pool, tok_y, tok_z, 1_000_000, 1_000_000));

    // The gapped pair is the synthetic fixture's own pools (not `pair_touch()`,
    // which addresses a different universe).
    let gapped: HashSet<Address> = [
        alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        alloy::primitives::address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    ]
    .into();
    let touched_by_victim: HashSet<Address> = [c, d_pool].into();
    let txs = txs(3);

    // Positive control: the gapped pair is detectable while in scope.
    let probe = probe_two(&pm, &ScanScope::Dirty(&gapped));
    assert!(
        !probe.is_empty(),
        "sanity: gapped pair must be detectable in scope"
    );

    let mut d = BackrunDetector::new(BLOCK);
    pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&gapped));

    let backruns = post(
        &mut d,
        &pm,
        VICTIM,
        &ScanScope::Dirty(&touched_by_victim),
        &txs,
    );
    assert!(
        backruns.is_empty(),
        "path outside the dirty scope must not be claimed: {backruns:?}"
    );
}

/// Two-hop and multi-hop both see the post-image gap -> exactly one row.
#[test]
fn dedups_same_gap_across_both_families() {
    // Same token pair on two "DEXes": the shape both families report as a
    // 2-pool cycle. Balanced at the pre-image — the gap must be opened by the
    // victim, or D3 (gap predates victim) correctly rejects the claim.
    let p1 = alloy::primitives::address!("aaaa000000000000000000000000000000000001");
    let p2 = alloy::primitives::address!("bbbb000000000000000000000000000000000002");
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        p1,
        usdc(),
        wmatic(),
        1_000_000_000_000,
        2_000_000_000_000_000_000,
    ));
    pm.add_pool(make_pool(
        p2,
        usdc(),
        wmatic(),
        1_000_000_000_000,
        2_000_000_000_000_000_000,
    ));
    let mut pm = pm.with_wrapped_native(wmatic());

    let touch: HashSet<Address> = [p1, p2].into();
    let txs = txs(3);
    let scope = ScanScope::Dirty(&touch);

    let mut d = BackrunDetector::new(BLOCK);
    let pre_opps = pre(&mut d, &pm, VICTIM, &scope);
    assert!(
        pre_opps.is_empty(),
        "balanced pools must not pre-detect: {pre_opps:?}"
    );

    // Victim swap drains p2's WMATIC side, opening a ~4x cross-pool gap.
    pm.update_from_logs(&[v2_swap(
        p2,
        1_020_000_000_000,
        0,
        0,
        1_000_000_000_000_000_000,
    )]);

    let keys = |opps: &[MevOpportunity]| -> HashSet<BackrunKey> {
        opps.iter().map(BackrunKey::from_opp).collect()
    };
    let two = keys(&probe_two(&pm, &scope));
    let multi = keys(&probe_multi(&pm, &scope));
    assert!(!two.is_empty(), "two_hop must see the gap: {two:?}");
    assert!(!multi.is_empty(), "multi_hop must see the gap: {multi:?}");
    assert!(
        two.intersection(&multi).next().is_some(),
        "fixture must be visible to both families (two={two:?}, multi={multi:?})"
    );

    let backruns = post(&mut d, &pm, VICTIM, &scope, &txs);
    assert_eq!(
        backruns.len(),
        1,
        "cross-family duplicate must collapse to one row: {backruns:?}"
    );
}

/// D2: the plain arb row for a claimed key is dropped; unrelated arbs and the
/// backrun row itself are retained.
#[test]
fn supersedes_plain_arb_in_same_block() {
    let pool_a = alloy::primitives::address!("00000000000000000000000000000000000000a1");
    let pool_b = alloy::primitives::address!("00000000000000000000000000000000000000b2");
    let other_a = alloy::primitives::address!("00000000000000000000000000000000000000c3");
    let other_b = alloy::primitives::address!("00000000000000000000000000000000000000d4");
    let token_in = alloy::primitives::address!("00000000000000000000000000000000000000e5");
    let token_out = alloy::primitives::address!("00000000000000000000000000000000000000f6");

    let mk = |strategy: Strategy, a: Address, b: Address| {
        let mut o = MevOpportunity::new(BLOCK, 3, strategy, a, TS);
        o.pool_b = b;
        o.token_in = token_in;
        o.token_out = token_out;
        o
    };

    let mut opps = vec![
        mk(Strategy::TwoHopArb, pool_a, pool_b),
        // Reversed orientation of the same path -> same normalized key.
        mk(Strategy::MultiHopArb, pool_b, pool_a),
        mk(Strategy::Backrun, pool_a, pool_b),
        // Unrelated pre-existing arb: must survive.
        mk(Strategy::TwoHopArb, other_a, other_b),
    ];

    suppress_superseded_arbs(&mut opps);

    assert_eq!(opps.len(), 2, "superseded arbs must be dropped: {opps:?}");
    assert_eq!(opps[0].strategy, Strategy::Backrun);
    assert_eq!(opps[1].strategy, Strategy::TwoHopArb);
    assert_eq!(opps[1].pool_a, other_a, "unrelated arb must be retained");
}

/// The same open gap is emitted once per block (reserve-aware `seen` map).
#[test]
fn emits_once_per_block_when_gap_persists() {
    let mut pm = balanced_pair();
    let touch = pair_touch();
    let txs = txs(3);
    let mut d = BackrunDetector::new(BLOCK);

    pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch));
    pm.update_from_logs(&[victim_skew()]);

    let first = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);
    assert_eq!(first.len(), 1, "first emission expected: {first:?}");

    // State unchanged: a later post pass must not re-emit the same gap.
    let second = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);
    assert!(
        second.is_empty(),
        "same open gap must not re-emit within the block: {second:?}"
    );
}

/// Gap closes under one victim and reopens under another: each flip gets its
/// own row (reserve change > 0.1% re-arms the dedup key).
#[test]
fn re_emits_after_gap_closes_and_reopens() {
    let mut pm = balanced_pair();
    let touch = pair_touch();
    let txs = txs(4);
    let mut d = BackrunDetector::new(BLOCK);

    // tx1: victim creates the gap -> row 1.
    pre(&mut d, &pm, 1, &ScanScope::Dirty(&touch));
    pm.update_from_logs(&[victim_skew()]);
    let r1 = post(&mut d, &pm, 1, &ScanScope::Dirty(&touch), &txs);
    assert_eq!(r1.len(), 1, "first victim must claim: {r1:?}");
    assert_eq!(r1[0].victim_tx_index, Some(1));

    // tx2: a later tx closes the gap (USDC pool skew back to parity).
    pre(&mut d, &pm, 2, &ScanScope::Dirty(&touch));
    pm.update_from_logs(&[v2_swap(
        matic_usdc_pool(),
        1_023_000_000_000,
        0,
        0,
        1_000_000_000_000_000_000,
    )]);
    let r2 = post(&mut d, &pm, 2, &ScanScope::Dirty(&touch), &txs);
    assert!(r2.is_empty(), "closed gap must not claim: {r2:?}");

    // tx3: the gap reopens with materially different reserves.
    pre(&mut d, &pm, 3, &ScanScope::Dirty(&touch));
    pm.update_from_logs(&[v2_swap(
        matic_usdt_pool(),
        2_100_000_000_000,
        0,
        0,
        500_000_000_000_000_000,
    )]);
    let r3 = post(&mut d, &pm, 3, &ScanScope::Dirty(&touch), &txs);
    assert_eq!(r3.len(), 1, "reopened gap must claim again: {r3:?}");
    assert_eq!(r3[0].victim_tx_index, Some(3));
}

/// Canonical id matches the explorer's realized form:
/// `Backrun|{anchor_pool}|source_tx:{victim}` (`explorer/canonical.rs`).
#[test]
fn canonical_id_matches_explorer_form() {
    let mut pm = balanced_pair();
    let touch = pair_touch();
    let txs = txs(3);
    let mut d = BackrunDetector::new(BLOCK);

    pre(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch));
    pm.update_from_logs(&[victim_skew()]);
    let backruns = post(&mut d, &pm, VICTIM, &ScanScope::Dirty(&touch), &txs);
    assert_eq!(backruns.len(), 1);

    let o = &backruns[0];
    let id = o
        .canonical_id
        .as_deref()
        .expect("backrun must carry a canonical id");
    let anchor = o
        .path
        .as_deref()
        .and_then(|p| p.iter().copied().find(|a| touch.contains(a)))
        .unwrap_or(o.pool_a);
    assert_eq!(
        id,
        format!("Backrun|{anchor:#x}|source_tx:{VICTIM}"),
        "id must match explorer/canonical.rs's realized Backrun form"
    );
}
