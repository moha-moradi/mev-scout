//! Real-block hunt recipes for explorer strategy scenarios.
//!
//! Synthetic fixtures live in [`super::scenarios`] (cfg(test)). This module is
//! the machine-readable companion to `docs/explorer_strategy_checklist.md`:
//! every scheduled plan item has a hunt recipe; Avalanche corpus seeds are
//! filled in as RECORD runs pin windows.
//!
//! Used by `tests/explorer_corpus.rs` (documentation + future tag assertions)
//! and by operators seeding new cases via `MEV_SCOUT_RECORD=*`.
use alloy::primitives::{address, Address};

/// How far a strategy scenario has been grounded on Avalanche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealSeedStatus {
    /// `explorer_corpus` has a pinned Avalanche window for this kind/tag.
    Seeded,
    /// Search recipe only — run RECORD to pin a window.
    Hunt,
    /// Protocol / fingerprint not expected on Avalanche (synthetic-only).
    SyntheticOnly,
}

/// One plan-row → synthetic test + optional real seed.
#[derive(Debug, Clone, Copy)]
pub struct StrategyScenarioTarget {
    /// Plan id (`P0.1`, `baseline.arb`, …).
    pub id: &'static str,
    /// Primary `details.tags` entry or MevKind string.
    pub label: &'static str,
    /// `#[test]` name in `explorer::scenarios`.
    pub syn_test: &'static str,
    pub real: RealSeedStatus,
    /// Avalanche inclusive block range when [`RealSeedStatus::Seeded`].
    pub seed_from: Option<u64>,
    pub seed_to: Option<u64>,
    /// Optional known searcher / liquidator for the seed window.
    pub seed_searcher: Option<Address>,
    /// One-line eth_getLogs / explorer hunt recipe.
    pub hunt: &'static str,
}

/// Full scheduled catalogue (baseline + P0–P3). Kept in sync with
/// `docs/explorer_strategy_checklist.md` by `tests/explorer_checklist_sync.rs`.
pub const STRATEGY_SCENARIO_TARGETS: &[StrategyScenarioTarget] = &[
    // ── baseline ────────────────────────────────────────────────────
    StrategyScenarioTarget {
        id: "baseline.arb",
        label: "arb_atomic",
        syn_test: "baseline_atomic_arb",
        real: RealSeedStatus::Seeded,
        seed_from: Some(95_681_722),
        seed_to: Some(95_682_322),
        seed_searcher: None,
        hunt: "corpus av-arb-*; dense multi-pool closed cycles",
    },
    StrategyScenarioTarget {
        id: "baseline.backrun",
        label: "backrun",
        syn_test: "baseline_backrun",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "third same-pool print; ETH verified @ 26264539 + 26264858 (2026-10-25 drpc, 2 seeds); AVAX 2026-10-25 (5001 blk, 98489000..=98494000, dwellir) 0",
    },
    StrategyScenarioTarget {
        id: "baseline.liquidation",
        label: "liquidation",
        syn_test: "baseline_liquidation",
        real: RealSeedStatus::Seeded,
        seed_from: Some(95_682_033),
        seed_to: Some(95_682_033),
        seed_searcher: Some(address!("d2a82f1bb41a950ad24829b2f483b1b10f3569dd")),
        hunt: "corpus av-liquidation-block",
    },
    StrategyScenarioTarget {
        id: "baseline.jit",
        label: "jit",
        syn_test: "baseline_jit",
        real: RealSeedStatus::Seeded,
        seed_from: Some(97_783_274),
        seed_to: Some(97_783_274),
        seed_searcher: None,
        hunt: "V3 Mint+Burn same owner/ticks with in-range swap",
    },
    StrategyScenarioTarget {
        id: "baseline.skim",
        label: "skim",
        syn_test: "baseline_skim",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "V2-like pair Transfer out without Swap/Sync/Mint/Burn; 2026-10-24 re-hunt 301 recent blocks: 0; AVAX 2026-10-25 (5001 blk, dwellir) 0",
    },
    // ── P0 ─────────────────────────────────────────────────────────────
    StrategyScenarioTarget {
        id: "P0.1",
        label: "flash_loan_liq",
        syn_test: "p0_1_flash_loan_liq",
        real: RealSeedStatus::Seeded,
        seed_from: Some(97_782_322),
        seed_to: Some(97_782_322),
        seed_searcher: None,
        hunt: "same tx Aave V3 FlashLoan + LiquidationCall (AVAX seed resolves to aave_v3)",
    },
    StrategyScenarioTarget {
        id: "P0.2",
        label: "benqi",
        syn_test: "p0_2_benqi_alias",
        real: RealSeedStatus::Seeded,
        seed_from: Some(97_850_836),
        seed_to: Some(97_850_836),
        seed_searcher: None,
        hunt: "LiquidateBorrow from qiToken in avalanche.liquidation_protocol_aliases (non-indexed data layout)",
    },
    StrategyScenarioTarget {
        id: "P0.4",
        label: "flash_arb",
        syn_test: "p0_4_flash_arb",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "arb_atomic with FlashLoan/Flash fact; filter RECORD on av-arb window",
    },
    // ── P1 ─────────────────────────────────────────────────────────────
    StrategyScenarioTarget {
        id: "P1.1",
        label: "interest_accrued",
        syn_test: "p1_1_interest_accrual",
        // Flag verified on Ethereum Morpho liq @ 26214788 (AVAX still Hunt).
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "LiquidationCall + ReserveDataUpdated lookback, no relevant AnswerUpdated (ETH verified @ 26214788)",
    },
    StrategyScenarioTarget {
        id: "P1.3",
        label: "long_tail",
        syn_test: "p1_3_long_tail",
        real: RealSeedStatus::Seeded,
        seed_from: Some(97_783_552),
        seed_to: Some(97_783_552),
        seed_searcher: None,
        hunt: "arb_atomic in av-arb window with non-blue-chip token on route",
    },
    StrategyScenarioTarget {
        id: "P1.4",
        label: "oracle_poke_block",
        syn_test: "p1_4_oracle_poke_block",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "LiquidationCall co-block with chainlink_feeds AnswerUpdated",
    },
    StrategyScenarioTarget {
        id: "P1.5",
        label: "keeper",
        syn_test: "p1_5_keeper_execution",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "Gelato ExecSuccess or Chainlink UpkeepPerformed/LogTriggered",
    },
    // ── P2 ─────────────────────────────────────────────────────────────
    StrategyScenarioTarget {
        id: "P2.3",
        label: "flash_providers",
        syn_test: "p2_3_flash_routing_stats",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "flash_loan_liq with ≥2 flash providers in one tx (rare)",
    },
    // ── P3 ─────────────────────────────────────────────────────────────
    StrategyScenarioTarget {
        id: "P3.1",
        label: "lb_bin_jit",
        syn_test: "p3_1_lb_bin_jit",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "LFJ/Pharaoh DLMM DepositedToBins+WithdrawnFromBins around swap",
    },
    StrategyScenarioTarget {
        id: "P3.2",
        label: "rate_provider_staleness",
        syn_test: "p3_2_balancer_staleness",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "Balancer+other-AMM arb, no TokenRateCacheUpdated in block",
    },
    StrategyScenarioTarget {
        id: "P3.5",
        label: "curve_imbalance",
        syn_test: "p3_5_curve_imbalance",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "Curve+non-Curve closed arb (Avalanche Curve volume thin → sparse)",
    },
    StrategyScenarioTarget {
        id: "P3.7",
        label: "gmx_adl_arb",
        syn_test: "p3_7_gmx_adl",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "GMX EventEmitter ADL in the same or a prior tx, then an arb touching a configured gmx_market_tokens index token",
    },
    StrategyScenarioTarget {
        id: "P3.12",
        label: "fot_arb|rebase_arb",
        syn_test: "p3_12_fot_rebase_arb",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "closed arb touching fot_tokens.json / rebase registry token",
    },
    StrategyScenarioTarget {
        id: "P3.13",
        label: "claim_and_sell",
        syn_test: "p3_13_claim_and_sell",
        real: RealSeedStatus::Seeded,
        seed_from: Some(97_781_738),
        seed_to: Some(97_781_738),
        seed_searcher: None,
        hunt: "Transfer from 0x0 + same-tx swap selling claimed token",
    },
    StrategyScenarioTarget {
        id: "P3.15",
        label: "epoch_transition",
        syn_test: "p3_15_epoch_transition",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "NotifyReward + both cycle legs on registry-matched Pharaoh/Blackhole pools; one touch or a timestamp alone is not enough",
    },
    StrategyScenarioTarget {
        id: "P3.16",
        label: "savax_rate_arb",
        syn_test: "p3_16_savax_rate_arb",
        real: RealSeedStatus::Hunt,
        seed_from: None,
        seed_to: None,
        seed_searcher: None,
        hunt: "sAVAX↔WAVAX closed arb vs getPooledAvaxByShares divergence",
    },
];

/// Targets still needing an Avalanche RECORD pin.
pub fn hunt_targets() -> impl Iterator<Item = &'static StrategyScenarioTarget> {
    STRATEGY_SCENARIO_TARGETS
        .iter()
        .filter(|t| t.real == RealSeedStatus::Hunt)
}

/// Targets with a pinned Avalanche corpus window.
pub fn seeded_targets() -> impl Iterator<Item = &'static StrategyScenarioTarget> {
    STRATEGY_SCENARIO_TARGETS
        .iter()
        .filter(|t| t.real == RealSeedStatus::Seeded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_seeded_targets_carry_seed_blocks() {
        for t in STRATEGY_SCENARIO_TARGETS {
            if t.real == RealSeedStatus::Seeded {
                assert!(
                    t.seed_from.is_some() && t.seed_to.is_some(),
                    "{} is Seeded but has no seed block",
                    t.id
                );
            } else {
                assert!(
                    t.seed_from.is_none() && t.seed_to.is_none(),
                    "{} is not Seeded but carries a seed block claim",
                    t.id
                );
            }
        }
    }

    #[test]
    fn catalogue_covers_syn_tests_uniquely() {
        let mut ids: Vec<&str> = STRATEGY_SCENARIO_TARGETS.iter().map(|t| t.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate strategy scenario ids");
        // 21 rows after the sandwich/frontrun kind, bundler, and bad-debt tag
        // retirements; a backrun is now a tag on baseline.arb, not a separate
        // baseline row.
        assert!(n >= 21, "expected full baseline+P0–P3 catalogue");
    }

    #[test]
    fn seeded_rows_have_block_range() {
        for t in seeded_targets() {
            assert!(t.seed_from.is_some() && t.seed_to.is_some(), "{}", t.id);
        }
    }

    #[test]
    fn hunt_rows_have_recipe() {
        for t in hunt_targets() {
            assert!(!t.hunt.is_empty(), "{}", t.id);
        }
    }
}
