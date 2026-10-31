//! Cross-check the strategy checklist, the seed catalogue, and the scenario
//! test names. The checklist is the human evidence log; the catalogue is the
//! machine copy of the same claims. A row that names a block no test covers is
//! a phantom claim, so seed/chain mismatches fail while status prose warns.
use std::collections::HashMap;

use mev_scout_core::explorer::scenario_targets::{RealSeedStatus, STRATEGY_SCENARIO_TARGETS};

const CHECKLIST: &str = include_str!("../../docs/explorer_strategy_checklist.md");
const SCENARIOS: &str = include_str!("../src/explorer/scenarios.rs");

#[derive(Debug)]
struct Row {
    id: String,
    status: String,
    chain: String,
    from: Option<u64>,
    to: Option<u64>,
}

fn checklist_rows() -> Vec<Row> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in CHECKLIST.lines() {
        if !in_table {
            if line.starts_with("| id | label | priority | status | chain |") {
                in_table = true;
            }
            continue;
        }
        if !line.trim_start().starts_with('|') {
            break;
        }
        if line.contains("---") {
            continue;
        }
        let cols: Vec<&str> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cols.len() < 8 {
            continue;
        }
        out.push(Row {
            id: cols[0].to_string(),
            status: cols[3].to_string(),
            chain: cols[4].to_string(),
            from: cols[5].parse().ok(),
            to: cols[6].parse().ok(),
        });
    }
    out
}

#[test]
fn every_catalogue_syn_test_names_a_real_function() {
    let mut missing = Vec::new();
    for t in STRATEGY_SCENARIO_TARGETS {
        // `syn_test` names a `#[test] fn` in scenarios.rs; a rename must not
        // leave the catalogue pointing at a phantom.
        if !SCENARIOS.contains(&format!("fn {}(", t.syn_test)) {
            missing.push(format!("{} -> {}", t.id, t.syn_test));
        }
    }
    assert!(missing.is_empty(), "phantom syn_test names: {missing:?}");
}

#[test]
fn checklist_and_catalogue_agree_on_avalanche_seeds() {
    let rows = checklist_rows();
    let mut by_id: HashMap<&str, &Row> = HashMap::new();
    for row in &rows {
        assert!(
            by_id.insert(row.id.as_str(), row).is_none(),
            "duplicate checklist id {}",
            row.id
        );
    }

    let mut missing = Vec::new();
    for t in STRATEGY_SCENARIO_TARGETS {
        let Some(row) = by_id.get(t.id) else {
            missing.push(t.id);
            continue;
        };
        if t.real == RealSeedStatus::Seeded && row.chain == "avalanche" {
            assert_eq!(
                row.from, t.seed_from,
                "{}: checklist seed_from disagrees with catalogue",
                t.id
            );
            assert_eq!(
                row.to, t.seed_to,
                "{}: checklist seed_to disagrees with catalogue",
                t.id
            );
        }
        // A verified Avalanche row must name the block it was verified on.
        if row.chain == "avalanche" && row.status == "verified" {
            assert!(
                row.from.is_some() && row.to.is_some(),
                "{}: verified Avalanche row has no seed block",
                t.id
            );
        }
    }
    assert!(
        missing.is_empty(),
        "catalogue ids absent from the checklist: {missing:?}"
    );

    // Cross-chain rows and status prose are allowed to exist only in the
    // checklist; surface them instead of silently ignoring them.
    let catalogue_ids: std::collections::HashSet<&str> =
        STRATEGY_SCENARIO_TARGETS.iter().map(|t| t.id).collect();
    for row in &rows {
        if !catalogue_ids.contains(row.id.as_str()) {
            eprintln!(
                "warning: checklist row {} [{} / {}] has no catalogue entry",
                row.id, row.status, row.chain
            );
        }
    }
}
