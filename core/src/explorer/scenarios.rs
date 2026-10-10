//! Synthetic per-strategy scenario catalogue for the explorer strategy
//! tracking plan (`docs/explorer_strategy_tracking_plan.md`).
//!
//! One `#[test]` per scheduled item (P0.1–P3.16) plus the baseline kinds.
//! Each test builds a deterministic [`BlockInput`] — a positive fingerprint and,
//! where the fingerprint has a plausible near-miss, a negative case — runs
//! [`classify_block`], and asserts the emitted label/tag **and** the exact `pnl`
//! plus `pnl_basis` from No RPC and no on-disk fixtures: every scenario is
//! self-contained, reproducible, and CI-safe (this is the deterministic
//! alternative to the RPC-gated replay in `tests/explorer_corpus.rs`).
//!
//! The module is only compiled under `cfg(test)` (see `explorer/mod.rs`).
//! Shared builders live in [`super::test_fixtures`].
use std::collections::HashMap;

use alloy::primitives::{address, b256, Address, U256};

use crate::explorer::classify::{classify_block, decode_tx_logs_with_aliases};
use crate::explorer::test_fixtures::{
    block, block_with_v2, classify_kind, event_of, flash_loan, has_tag, jit, kinds, liquidation,
    pnl_basis, swap, swap_at_tick, transfer, tx, ATK, FOT, MARKET, POOL_A, POOL_B, REBASE, SAVAX,
    TOKA, USDC, VICTIM, WNATIVE,
};
use crate::explorer::types::{
    Amm, Confidence, EpochRewardFact, GmxEventFact, JitFact, KeeperFact, MevKind, OracleUpdateFact,
    ReserveDataFact, UserOpFact,
};

// ── Baseline kinds ──────────────────────────────────────────────────────

/// Baseline `ArbAtomic`: two-pool closed cycle → R, exact pnl. Negative:
/// single-hop residual with the mevlive-parity fallback off emits nothing.
#[test]
fn baseline_atomic_arb() {
    let swaps = vec![
        swap(POOL_A, USDC, TOKA, 100, 200),
        swap(POOL_B, TOKA, USDC, 200, 110),
    ];
    let transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let ev = classify_kind(
        &block(vec![tx(0, ATK, true, swaps, transfers)]),
        MevKind::ArbAtomic,
    );
    assert_eq!(ev.confidence, Confidence::Exact);
    assert_eq!(ev.profit_token, Some(USDC));
    assert_eq!(ev.profit_amount, Some(U256::from(10)));
    assert_eq!(pnl_basis(&ev), Some("R"));

    // Negative: not a closed cycle and parity disabled → no event at all.
    let mut input = block(vec![tx(
        0,
        ATK,
        true,
        vec![swap(POOL_A, USDC, TOKA, 100, 200)],
        vec![transfer(0, USDC, ATK, POOL_A, 100)],
    )]);
    input.arb_likely_parity = false;
    assert!(classify_block(&input).is_empty());
}

/// Baseline `Sandwich`: canonical front/victim/back bundle → R. Negative: a
/// plain attacker round-trip with no third-party victim is not a sandwich.
#[test]
fn baseline_sandwich() {
    let t0 = tx(
        0,
        ATK,
        true,
        vec![swap(POOL_A, USDC, TOKA, 100, 200)],
        vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ],
    );
    let t1 = tx(
        1,
        VICTIM,
        true,
        vec![swap(POOL_A, USDC, TOKA, 200, 350)],
        vec![
            transfer(0, USDC, VICTIM, POOL_A, 200),
            transfer(1, TOKA, POOL_A, VICTIM, 350),
        ],
    );
    let t2 = tx(
        2,
        ATK,
        true,
        vec![swap(POOL_A, TOKA, USDC, 350, 195)],
        vec![
            transfer(0, TOKA, ATK, POOL_A, 350),
            transfer(1, USDC, POOL_A, ATK, 195),
        ],
    );
    let ev = classify_kind(&block(vec![t0, t1, t2]), MevKind::Sandwich);
    assert_eq!(ev.searcher, ATK);
    assert_eq!(ev.profit_amount, Some(U256::from(95)));
    assert_eq!(ev.confidence, Confidence::Exact);
    assert_eq!(pnl_basis(&ev), Some("R"));

    // Negative: attacker buys then sells with no intervening victim.
    let neg0 = tx(
        0,
        ATK,
        true,
        vec![swap(POOL_A, USDC, TOKA, 100, 200)],
        vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ],
    );
    let neg1 = tx(
        1,
        ATK,
        true,
        vec![swap(POOL_A, TOKA, USDC, 200, 95)],
        vec![
            transfer(0, TOKA, ATK, POOL_A, 200),
            transfer(1, USDC, POOL_A, ATK, 95),
        ],
    );
    assert!(!kinds(&classify_block(&block(vec![neg0, neg1]))).contains(&MevKind::Sandwich));
}

/// Baseline `Frontrun`: searcher moves the pool, a third party executes
/// degraded, searcher closes profitably → R. Negative: no degradation.
#[test]
fn baseline_frontrun() {
    for (name, victim_out, expect) in [
        ("degraded victim", 150u64, true),
        ("flat victim", 200, false),
    ] {
        let f = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let v = tx(
            1,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, victim_out)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, victim_out),
            ],
        );
        let c = tx(
            2,
            ATK,
            true,
            vec![swap(POOL_B, TOKA, USDC, 200, 250)],
            vec![
                transfer(0, TOKA, ATK, POOL_B, 200),
                transfer(1, USDC, POOL_B, ATK, 250),
            ],
        );
        let events = classify_block(&block(vec![f, v, c]));
        assert_eq!(
            kinds(&events).contains(&MevKind::Frontrun),
            expect,
            "{name}"
        );
        if expect {
            let ev = event_of(&events, MevKind::Frontrun);
            assert_eq!(ev.profit_amount, Some(U256::from(150)));
            assert_eq!(pnl_basis(ev), Some("R"));
        }
    }
}

/// Baseline `Backrun`: a large third-party move followed by the searcher's
/// profitable opposite cycle → R. Negative: same trade without the pre-move
/// reference is not attributed.
#[test]
fn baseline_backrun() {
    for (name, with_reference, expect) in
        [("with pre-move ref", true, true), ("no ref", false, false)]
    {
        let mut txs = Vec::new();
        if with_reference {
            txs.push(tx(
                0,
                VICTIM,
                true,
                vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                vec![
                    transfer(0, TOKA, VICTIM, POOL_A, 100),
                    transfer(1, USDC, POOL_A, VICTIM, 110),
                ],
            ));
        }
        let move_idx = txs.len() as u64;
        txs.push(tx(
            move_idx,
            MARKET,
            true,
            vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
            vec![
                transfer(0, USDC, MARKET, POOL_A, 1000),
                transfer(1, TOKA, POOL_A, MARKET, 100),
            ],
        ));
        let back_idx = txs.len() as u64;
        txs.push(tx(
            back_idx,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 100, 120),
                swap(POOL_B, USDC, TOKA, 120, 130),
            ],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 100),
                transfer(1, USDC, POOL_A, ATK, 120),
                transfer(2, USDC, ATK, POOL_B, 120),
                transfer(3, TOKA, POOL_B, ATK, 130),
            ],
        ));
        let events = classify_block(&block(txs));
        assert_eq!(kinds(&events).contains(&MevKind::Backrun), expect, "{name}");
        if expect {
            let ev = event_of(&events, MevKind::Backrun);
            assert_eq!(ev.searcher, ATK);
            assert_eq!(pnl_basis(ev), Some("R"));
        }
    }
}

/// Baseline `Liquidation`: Aave event → O basis, exact collateral shard.
/// Negative: a block with no liquidation facts emits no Liquidation event.
#[test]
fn baseline_liquidation() {
    let mut t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
    let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
    assert_eq!(ev.searcher, ATK);
    assert_eq!(ev.profit_token, Some(USDC));
    assert_eq!(ev.profit_amount, Some(U256::from(500)));
    assert_eq!(ev.confidence, Confidence::Exact);
    assert_eq!(pnl_basis(&ev), Some("O"));

    assert!(!kinds(&classify_block(&block(vec![tx(
        0,
        ATK,
        true,
        vec![],
        vec![]
    )])))
    .contains(&MevKind::Liquidation));
}

/// Baseline `Jit`: mint + burn around an in-range swap → F basis. Negative:
/// the swap lands outside the tick range → ordinary LP activity.
#[test]
fn baseline_jit() {
    let mut t0 = tx(
        0,
        ATK,
        true,
        vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 0)],
        vec![],
    );
    t0.jit = vec![jit(0, true)];
    let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
    t1.jit = vec![jit(1, false)];
    let ev = classify_kind(&block(vec![t0, t1]), MevKind::Jit);
    assert_eq!(ev.searcher, ATK);
    assert_eq!(pnl_basis(&ev), Some("F"));

    let mut n0 = tx(
        0,
        ATK,
        true,
        vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 200)],
        vec![],
    );
    n0.jit = vec![jit(0, true)];
    let mut n1 = tx(1, VICTIM, true, vec![], vec![]);
    n1.jit = vec![jit(1, false)];
    assert!(!kinds(&classify_block(&block(vec![n0, n1]))).contains(&MevKind::Jit));
}

/// Baseline `Skim`: V2-like pair outbound transfer with no swap → R.
/// Negative: the same pair also has a Swap in the tx → not a skim.
#[test]
fn baseline_skim() {
    let t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 42)],
    );
    let ev = classify_kind(&block_with_v2(vec![t], &[POOL_A]), MevKind::Skim);
    assert_eq!(ev.searcher, ATK);
    assert_eq!(ev.profit_amount, Some(U256::from(42)));
    assert_eq!(pnl_basis(&ev), Some("R"));

    let neg = tx(
        0,
        ATK,
        true,
        vec![swap(POOL_A, USDC, TOKA, 100, 200)],
        vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ],
    );
    assert!(!kinds(&classify_block(&block_with_v2(vec![neg], &[POOL_A]))).contains(&MevKind::Skim));
}

// ── Priority 0 ─────────────────────────────────────────────────────────────

/// P0.1 — flash-loan atomic liquidation tag. Positive asserts the tag, the
/// O-basis liquidation amount and the F-basis flash premium; negative is a
/// flash loan without a liquidation.
#[test]
fn p0_1_flash_loan_liq() {
    let provider = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let mut t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
    t.flashloans = vec![flash_loan(provider)];
    let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
    assert!(has_tag(std::slice::from_ref(&ev), "flash_loan_liq"));
    assert_eq!(pnl_basis(&ev), Some("O"));
    assert_eq!(ev.details["flash_provider"], serde_json::json!("aave_v3"));
    assert_eq!(ev.details["flash_premium"], serde_json::json!("5"));
    assert_eq!(
        ev.details["pnl"]["flash_premium_basis"],
        serde_json::json!("F")
    );
    assert_eq!(ev.profit_amount, Some(U256::from(500)));

    let mut neg = tx(0, ATK, true, vec![], vec![]);
    neg.flashloans = vec![flash_loan(provider)];
    let events = classify_block(&block(vec![neg]));
    assert!(!events.iter().any(|e| e.kind == MevKind::Liquidation));
    assert!(!has_tag(&events, "flash_loan_liq"));
}

/// P0.2 — Benqi relabel via the emitter-alias registry. Positive decodes the
/// raw Compound-V2 `LiquidateBorrow` topic through the alias to `benqi`; the
/// negative uses the same log with no alias (stays `compound_v2`). Both keep
/// the Compound-family O basis.
#[test]
fn p0_2_benqi_alias() {
    use crate::chain::events::COMPOUND_V2_LIQUIDATE_BORROW_TOPIC;
    use crate::data::LogData;

    let qi_avax = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
    let qi_eth = address!("334ad834cd4481bb02d09615e7c11a00579a7909");
    let liquidator_topic =
        b256!("0000000000000000000000001000000000000000000000000000000000000001"); // ATK
    let borrower_topic = b256!("0000000000000000000000002000000000000000000000000000000000000002"); // VICTIM
    let mut data = vec![0u8; 96];
    data[24..32].copy_from_slice(&300u64.to_be_bytes());
    data[44..64].copy_from_slice(qi_eth.as_slice());
    data[88..96].copy_from_slice(&500u64.to_be_bytes());
    let liq_log = LogData {
        address: qi_avax,
        topics: vec![
            *COMPOUND_V2_LIQUIDATE_BORROW_TOPIC,
            liquidator_topic,
            borrower_topic,
        ],
        data: alloy::primitives::Bytes::from(data),
    };

    let aliases: HashMap<Address, &'static str> = HashMap::from([(qi_avax, "benqi")]);
    let (_, _, liquidations, ..) =
        decode_tx_logs_with_aliases(0, std::slice::from_ref(&liq_log), &HashMap::new(), &aliases);
    assert_eq!(liquidations.len(), 1);
    assert_eq!(liquidations[0].protocol, "benqi");

    let (_, _, unaliased, ..) =
        decode_tx_logs_with_aliases(0, &[liq_log], &HashMap::new(), &HashMap::new());
    assert_eq!(unaliased[0].protocol, "compound_v2");

    let mut t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, qi_eth, POOL_A, ATK, 500)],
    );
    t.liquidations = liquidations;
    let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
    assert_eq!(ev.details["protocol"], serde_json::json!("benqi"));
    assert_eq!(pnl_basis(&ev), Some("O"));
    assert_eq!(
        ev.details["pnl"]["collateral_amount"],
        serde_json::json!("500")
    );
    assert_eq!(ev.profit_amount, Some(U256::from(500)));
    assert_eq!(ev.confidence, Confidence::Exact);
}

/// P0.4 — flash-swap / flash-loan flag on an atomic arb. Positive carries
/// `flash_arb` and nets the premium (R); negative is an unfunded closed cycle.
#[test]
fn p0_4_flash_arb() {
    let provider = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let swaps = vec![
        swap(POOL_A, USDC, TOKA, 1000, 1200),
        swap(POOL_B, TOKA, USDC, 1200, 1010),
    ];
    let transfers = vec![
        transfer(0, USDC, provider, ATK, 1000),
        transfer(1, USDC, ATK, POOL_A, 1000),
        transfer(2, TOKA, POOL_A, ATK, 1200),
        transfer(3, TOKA, ATK, POOL_B, 1200),
        transfer(4, USDC, POOL_B, ATK, 1010),
        transfer(5, USDC, ATK, provider, 1005),
    ];
    let mut t = tx(0, ATK, true, swaps, transfers);
    t.flashloans = vec![flash_loan(provider)];
    let ev = classify_kind(&block(vec![t]), MevKind::ArbAtomic);
    assert!(has_tag(std::slice::from_ref(&ev), "flash_arb"));
    assert_eq!(pnl_basis(&ev), Some("R"));
    assert_eq!(ev.details["pnl"]["profit_amount"], serde_json::json!("5"));
    assert_eq!(ev.profit_amount, Some(U256::from(5)));
    assert_eq!(ev.flashloan_fee_wei, Some(U256::from(5)));

    let plain = vec![
        swap(POOL_A, USDC, TOKA, 100, 200),
        swap(POOL_B, TOKA, USDC, 200, 110),
    ];
    let plain_transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let ev = classify_kind(
        &block(vec![tx(0, ATK, true, plain, plain_transfers)]),
        MevKind::ArbAtomic,
    );
    assert!(!has_tag(std::slice::from_ref(&ev), "flash_arb"));
}

// ── Priority 1 ─────────────────────────────────────────────────────────────

/// P1.1 — interest-accrual cause label (O P&L unchanged). Positive:
/// `ReserveDataUpdated` for the debt with a flat price; negative: the debt
/// feed itself pokes in the same block.
#[test]
fn p1_1_interest_accrual() {
    let feed = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let flat = |with_poke: bool| {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 500)],
        );
        t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
        t.reserve_updates = vec![ReserveDataFact {
            tx_index: 0,
            log_index: 1,
            reserve: WNATIVE,
            variable_borrow_rate: U256::from(1),
        }];
        if with_poke {
            t.oracle_updates = vec![OracleUpdateFact {
                tx_index: 0,
                log_index: 2,
                feed,
                answer: 100,
            }];
        }
        let mut input = block(vec![t]);
        input.chainlink_feeds = HashMap::from([(feed, WNATIVE)]);
        classify_kind(&input, MevKind::Liquidation)
    };

    let ev = flat(false);
    assert_eq!(ev.details["interest_accrued"], serde_json::json!(true));
    assert_eq!(pnl_basis(&ev), Some("O"));

    let ev = flat(true);
    assert_eq!(ev.details["interest_accrued"], serde_json::json!(false));
    assert_eq!(ev.details["oracle_poke_block"], serde_json::json!(true));
}

/// P1.3 — long-tail arb label. Positive: a closed cycle through a
/// non-blue-chip token (R). Negative: blue-chip-only route.
#[test]
fn p1_3_long_tail() {
    let swaps = vec![
        swap(POOL_A, USDC, TOKA, 100, 200),
        swap(POOL_B, TOKA, USDC, 200, 110),
    ];
    let transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let ev = classify_kind(
        &block(vec![tx(0, ATK, true, swaps, transfers)]),
        MevKind::ArbAtomic,
    );
    assert!(has_tag(std::slice::from_ref(&ev), "long_tail"));
    assert_eq!(pnl_basis(&ev), Some("R"));
    assert_eq!(ev.profit_amount, Some(U256::from(10)));

    let blue = vec![
        swap(POOL_A, USDC, WNATIVE, 100, 200),
        swap(POOL_B, WNATIVE, USDC, 200, 110),
    ];
    let blue_transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, WNATIVE, POOL_A, ATK, 200),
        transfer(2, WNATIVE, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let mut input = block(vec![tx(0, ATK, true, blue, blue_transfers)]);
    input.profit_policy.priority = vec![USDC, WNATIVE];
    let ev = classify_kind(&input, MevKind::ArbAtomic);
    assert!(!has_tag(std::slice::from_ref(&ev), "long_tail"));
}

/// P1.4 — oracle-latency liquidation co-block. Positive: the block also
/// contains a Chainlink poke for the relevant feed. Negative: no poke.
#[test]
fn p1_4_oracle_poke_block() {
    let feed = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let build = |with_poke: bool| {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 500)],
        );
        t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
        if with_poke {
            t.oracle_updates = vec![OracleUpdateFact {
                tx_index: 0,
                log_index: 1,
                feed,
                answer: 100,
            }];
        }
        let mut input = block(vec![t]);
        input.chainlink_feeds = HashMap::from([(feed, USDC)]);
        classify_kind(&input, MevKind::Liquidation)
    };
    let ev = build(true);
    assert_eq!(ev.details["oracle_poke_block"], serde_json::json!(true));
    assert_eq!(pnl_basis(&ev), Some("O"));
    let ev = build(false);
    assert_eq!(ev.details["oracle_poke_block"], serde_json::json!(false));
}

/// P1.5 — keeper / automation execution. Positive: Gelato `ExecSuccess` →
/// `keeper` tag, F basis and the explicit fee amount. Negative: no keeper
/// facts ⇒ no keeper tag.
#[test]
fn p1_5_keeper_execution() {
    let mut t = tx(0, ATK, true, vec![], vec![]);
    t.keepers = vec![KeeperFact {
        tx_index: 0,
        log_index: 0,
        protocol: "gelato",
        emitter: address!("cccccccccccccccccccccccccccccccccccccccc"),
        fee: Some(U256::from(42)),
        fee_token: Some(USDC),
    }];
    let events = classify_block(&block(vec![t]));
    assert!(has_tag(&events, "keeper"));
    let ev = event_of(&events, MevKind::Unknown);
    assert_eq!(ev.details["protocol"], serde_json::json!("gelato"));
    assert_eq!(pnl_basis(ev), Some("F"));
    assert_eq!(ev.profit_amount, Some(U256::from(42)));

    let events = classify_block(&block(vec![tx(0, ATK, true, vec![], vec![])]));
    assert!(!has_tag(&events, "keeper"));
}

// ── Priority 2 ─────────────────────────────────────────────────────────────

/// P2.3 — flash-loan routing stats. Positive: two providers in one
/// liquidation tx are recorded under `flash_providers` (O basis preserved).
/// Negative: a single provider does not emit the routing list.
#[test]
fn p2_3_flash_routing_stats() {
    let p1 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1");
    let p2 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2");
    let mut t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
    let mut fl2 = flash_loan(p2);
    fl2.protocol = "balancer";
    fl2.fee = Some(U256::from(3));
    t.flashloans = vec![flash_loan(p1), fl2];
    let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
    assert_eq!(
        ev.details["flash_providers"].as_array().map(|a| a.len()),
        Some(2)
    );
    assert_eq!(pnl_basis(&ev), Some("O"));

    let mut single = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    single.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
    single.flashloans = vec![flash_loan(p1)];
    let ev = classify_kind(&block(vec![single]), MevKind::Liquidation);
    assert!(ev.details.get("flash_providers").is_none());
}

// ── Priority 3 ─────────────────────────────────────────────────────────────

/// P3.1 — Trader Joe V2 LB bin-JIT. Positive: bin-AMM mint/burn around a
/// same-pool swap (no tick on the swap) → `Jit` with `bin_amm` and the F-basis
/// `lb_bin_jit` tag. Negative: mint with no matching burn.
#[test]
fn p3_1_lb_bin_jit() {
    let bin = |tx_index: u64, is_mint: bool| JitFact {
        tx_index,
        log_index: 0,
        pool: POOL_A,
        owner: ATK,
        tick_lower: 8000,
        tick_upper: 8010,
        is_mint,
        liquidity: 2,
        amount0: U256::from(10),
        amount1: U256::from(5),
        bin_amm: true,
    };
    let mut t0 = tx(0, ATK, true, vec![], vec![]);
    t0.jit = vec![bin(0, true)];
    let t1 = tx(
        1,
        MARKET,
        true,
        vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
        vec![],
    );
    let mut t2 = tx(2, ATK, true, vec![], vec![]);
    t2.jit = vec![bin(2, false)];
    let ev = classify_kind(&block(vec![t0, t1, t2]), MevKind::Jit);
    assert_eq!(ev.details["bin_amm"], serde_json::json!(true));
    assert_eq!(pnl_basis(&ev), Some("F"));
    assert!(has_tag(std::slice::from_ref(&ev), "lb_bin_jit"));

    let mut mint_only = tx(0, ATK, true, vec![], vec![]);
    mint_only.jit = vec![bin(0, true)];
    assert!(!kinds(&classify_block(&block(vec![mint_only]))).contains(&MevKind::Jit));
}

/// P3.2 — Balancer rate-provider staleness (inferred cause, R P&L).
/// Positive: Balancer + V2 cross-venue cycle with no TokenRateCacheUpdated.
/// Negative: plain V2-only cycle, or Balancer cycle with a rate-cache refresh.
#[test]
fn p3_2_balancer_staleness() {
    let cross = |amm0: Amm, amm1: Amm| {
        let mut s0 = swap(POOL_A, USDC, TOKA, 100, 200);
        s0.amm = amm0;
        let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
        s1.amm = amm1;
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        classify_kind(
            &block(vec![tx(0, ATK, true, vec![s0, s1], transfers)]),
            MevKind::ArbAtomic,
        )
    };
    let ev = cross(Amm::Balancer, Amm::V2);
    assert!(has_tag(
        std::slice::from_ref(&ev),
        "rate_provider_staleness"
    ));
    assert_eq!(pnl_basis(&ev), Some("R"));
    let ev = cross(Amm::V2, Amm::V2);
    assert!(!has_tag(
        std::slice::from_ref(&ev),
        "rate_provider_staleness"
    ));

    let mut s0 = swap(POOL_A, USDC, TOKA, 100, 200);
    s0.amm = Amm::Balancer;
    let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
    s1.amm = Amm::V2;
    let transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let mut t = tx(0, ATK, true, vec![s0, s1], transfers);
    t.rate_cache_updates = vec![crate::explorer::types::RateCacheFact {
        tx_index: 0,
        log_index: 9,
        pool: address!("cccccccccccccccccccccccccccccccccccccccc"),
        token_index: 0,
        rate: U256::from(1),
    }];
    let ev = classify_kind(&block(vec![t]), MevKind::ArbAtomic);
    assert!(!has_tag(
        std::slice::from_ref(&ev),
        "rate_provider_staleness"
    ));
}

/// P3.5 — Curve pool imbalance arb. Positive: Curve + other venue.
/// Negative: Curve-only or non-Curve route.
#[test]
fn p3_5_curve_imbalance() {
    let cross = |amm0: Amm, amm1: Amm| {
        let mut s0 = swap(POOL_A, USDC, TOKA, 100, 200);
        s0.amm = amm0;
        let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
        s1.amm = amm1;
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        classify_kind(
            &block(vec![tx(0, ATK, true, vec![s0, s1], transfers)]),
            MevKind::ArbAtomic,
        )
    };
    let ev = cross(Amm::Curve, Amm::V2);
    assert!(has_tag(std::slice::from_ref(&ev), "curve_imbalance"));
    assert_eq!(pnl_basis(&ev), Some("R"));
    let ev = cross(Amm::Curve, Amm::Curve);
    assert!(!has_tag(std::slice::from_ref(&ev), "curve_imbalance"));
    let ev = cross(Amm::V2, Amm::V2);
    assert!(!has_tag(std::slice::from_ref(&ev), "curve_imbalance"));
}

/// P3.7 — GMX V2 ADL-adjacent arb. Positive: a co-block EventEmitter ADL
/// signal tags the arb. Negative: no GMX signal.
#[test]
fn p3_7_gmx_adl() {
    let build = |with_gmx: bool| {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let mut t = tx(0, ATK, true, swaps, transfers);
        if with_gmx {
            t.gmx_events = vec![GmxEventFact {
                tx_index: 0,
                log_index: 5,
                emitter: address!("db17b233827785b5ea1ad91c9bd7d4dc478b9389"),
                kind: "adl",
            }];
        }
        classify_kind(&block(vec![t]), MevKind::ArbAtomic)
    };
    let ev = build(true);
    assert!(has_tag(std::slice::from_ref(&ev), "gmx_adl_arb"));
    assert_eq!(pnl_basis(&ev), Some("R"));
    let ev = build(false);
    assert!(!has_tag(std::slice::from_ref(&ev), "gmx_adl_arb"));
}

/// P3.11 — ERC-4337 bundler execution. Positive: a successful user op →
/// `bundler` tag with R basis and the actual gas cost. Negative: a reverted
/// user op is ignored.
#[test]
fn p3_11_erc4337_bundler() {
    let op = |success: bool| UserOpFact {
        tx_index: 0,
        log_index: 0,
        entry_point: address!("5ff137d4b0fdcd49dca30c7cf57e578a026d2789"),
        sender: VICTIM,
        paymaster: Address::ZERO,
        actual_gas_cost: U256::from(12_000),
        success,
    };
    let mut t = tx(0, ATK, true, vec![], vec![]);
    t.user_ops = vec![op(true)];
    let events = classify_block(&block(vec![t]));
    assert!(has_tag(&events, "bundler"));
    let ev = event_of(&events, MevKind::Unknown);
    assert_eq!(pnl_basis(ev), Some("R"));
    assert_eq!(ev.profit_amount, Some(U256::from(12_000)));

    let mut neg = tx(0, ATK, true, vec![], vec![]);
    neg.user_ops = vec![op(false)];
    assert!(!has_tag(&classify_block(&block(vec![neg])), "bundler"));
}

/// P3.12 — rebase / FoT token arb. Positive: a closed cycle touching a
/// registry FoT token (`fot_arb`) or rebase token (`rebase_arb`), R basis.
/// Negative: a plain USDC↔TOKA cycle carries neither tag.
#[test]
fn p3_12_fot_rebase_arb() {
    let cycle = |token: Address| {
        let swaps = vec![
            swap(POOL_A, USDC, token, 100, 200),
            swap(POOL_B, token, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, token, POOL_A, ATK, 200),
            transfer(2, token, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        classify_kind(
            &block(vec![tx(0, ATK, true, swaps, transfers)]),
            MevKind::ArbAtomic,
        )
    };
    let ev = cycle(FOT);
    assert!(has_tag(std::slice::from_ref(&ev), "fot_arb"));
    assert_eq!(pnl_basis(&ev), Some("R"));
    let ev = cycle(REBASE);
    assert!(has_tag(std::slice::from_ref(&ev), "rebase_arb"));

    let plain = cycle(TOKA);
    assert!(!has_tag(std::slice::from_ref(&plain), "fot_arb"));
    assert!(!has_tag(std::slice::from_ref(&plain), "rebase_arb"));
}

/// P3.13 — airdrop claim-and-sell. Positive: a zero-from mint sold by a
/// same-tx swap is tagged. Negative: a held claim (no sell) is ignored.
#[test]
fn p3_13_claim_and_sell() {
    let swaps = vec![
        swap(POOL_A, TOKA, USDC, 100, 110),
        swap(POOL_B, USDC, TOKA, 50, 40),
    ];
    let transfers = vec![
        transfer(0, TOKA, Address::ZERO, ATK, 1000),
        transfer(1, TOKA, ATK, POOL_A, 100),
        transfer(2, USDC, POOL_A, ATK, 110),
    ];
    let events = classify_block(&block(vec![tx(0, ATK, true, swaps, transfers)]));
    assert!(has_tag(&events, "claim_and_sell"));

    let held = vec![transfer(0, TOKA, Address::ZERO, ATK, 1000)];
    let events = classify_block(&block(vec![tx(0, ATK, true, vec![], held)]));
    assert!(!has_tag(&events, "claim_and_sell"));
}

/// P3.14 — bad-debt / near-insolvent liquidation attribution. Positive:
/// `badDebtAssets > 0` → `bad_debt_liq` tag, O basis unchanged. Negative:
/// zero bad debt.
#[test]
fn p3_14_bad_debt_liq() {
    let mut t = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    let mut liq = liquidation("morpho_blue", ATK, USDC, WNATIVE, 500, 300);
    liq.bad_debt_assets = U256::from(10);
    t.liquidations = vec![liq];
    let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
    assert!(has_tag(std::slice::from_ref(&ev), "bad_debt_liq"));
    assert_eq!(ev.details["bad_debt"], serde_json::json!(true));
    assert_eq!(ev.details["post_liq_insolvent"], serde_json::json!(true));
    assert_eq!(pnl_basis(&ev), Some("O"));

    let mut clean = tx(
        0,
        ATK,
        true,
        vec![],
        vec![transfer(0, USDC, POOL_A, ATK, 500)],
    );
    clean.liquidations = vec![liquidation("morpho_blue", ATK, USDC, WNATIVE, 500, 300)];
    let ev = classify_kind(&block(vec![clean]), MevKind::Liquidation);
    assert!(!has_tag(std::slice::from_ref(&ev), "bad_debt_liq"));
}

/// P3.15 — Pharaoh / Blackhole epoch-transition arb. Positive: a co-block
/// `NotifyReward` plus a venue AMM leg (or a Thursday-00:00-UTC timestamp).
/// Negative: plain V2 route away from the boundary.
#[test]
fn p3_15_epoch_transition() {
    let solidly_cycle = |epoch_reward: bool| {
        let mut s0 = swap(POOL_A, USDC, TOKA, 100, 200);
        s0.amm = Amm::Solidly;
        let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
        s1.amm = Amm::Solidly;
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let mut t = tx(0, ATK, true, vec![s0, s1], transfers);
        if epoch_reward {
            t.epoch_rewards = vec![EpochRewardFact {
                tx_index: 0,
                log_index: 9,
                emitter: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                reward_token: USDC,
                amount: U256::from(1),
            }];
        }
        classify_kind(&block(vec![t]), MevKind::ArbAtomic)
    };
    let ev = solidly_cycle(true);
    assert!(has_tag(std::slice::from_ref(&ev), "epoch_transition"));
    assert_eq!(pnl_basis(&ev), Some("R"));

    // Timestamp-only path: 2024-01-04 00:00:00 UTC (Thursday) + V3 leg.
    let mut s0 = swap(POOL_A, USDC, TOKA, 100, 200);
    s0.amm = Amm::V3;
    let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
    s1.amm = Amm::V3;
    let transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let mut input = block(vec![tx(0, ATK, true, vec![s0, s1], transfers)]);
    input.ts = 1_704_326_400;
    let ev = classify_kind(&input, MevKind::ArbAtomic);
    assert!(has_tag(std::slice::from_ref(&ev), "epoch_transition"));

    // Negative: V2 route at the default (non-boundary) timestamp.
    let plain = vec![
        swap(POOL_A, USDC, TOKA, 100, 200),
        swap(POOL_B, TOKA, USDC, 200, 110),
    ];
    let plain_transfers = vec![
        transfer(0, USDC, ATK, POOL_A, 100),
        transfer(1, TOKA, POOL_A, ATK, 200),
        transfer(2, TOKA, ATK, POOL_B, 200),
        transfer(3, USDC, POOL_B, ATK, 110),
    ];
    let ev = classify_kind(
        &block(vec![tx(0, ATK, true, plain, plain_transfers)]),
        MevKind::ArbAtomic,
    );
    assert!(!has_tag(std::slice::from_ref(&ev), "epoch_transition"));
}

/// P3.16 — sAVAX rate arb. Positive: sAVAX ↔ wrapped-native route. Negative:
/// sAVAX present but no wrapped-native leg.
#[test]
fn p3_16_savax_rate_arb() {
    let build = |quote: Address| {
        let swaps = vec![
            swap(POOL_A, SAVAX, quote, 100, 110),
            swap(POOL_B, quote, SAVAX, 110, 105),
        ];
        let transfers = vec![
            transfer(0, SAVAX, ATK, POOL_A, 100),
            transfer(1, quote, POOL_A, ATK, 110),
            transfer(2, quote, ATK, POOL_B, 110),
            transfer(3, SAVAX, POOL_B, ATK, 105),
        ];
        let mut input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        input.savax = Some(SAVAX);
        input.profit_policy.priority = vec![SAVAX, WNATIVE, USDC];
        classify_kind(&input, MevKind::ArbAtomic)
    };
    let ev = build(WNATIVE);
    assert!(has_tag(std::slice::from_ref(&ev), "savax_rate_arb"));
    assert_eq!(pnl_basis(&ev), Some("R"));

    let ev = build(USDC);
    assert!(!has_tag(std::slice::from_ref(&ev), "savax_rate_arb"));
}
