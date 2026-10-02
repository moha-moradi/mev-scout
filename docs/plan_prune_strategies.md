# Prune plan: Sandwich + JitArb + Liquidation from the execution path

Status: **implemented** (2026-10-02) — Phases 1–5 done, `cargo test` +
`cargo clippy --all-targets -- -D warnings` green.
Created: 2026-10-02

## Goal

Reduce the live/execution engine to strategies that are actually executable
without capital, flash-loan-aware, and modelled honestly. Keep the explorer
subtree intact — it is a separate system with real on-chain evidence.

| Strategy | live | explorer | Reason |
|---|---|---|---|
| `two_hop_arb` | keep | keep | flash-loan-aware (`two_hop.rs:144`) |
| `multi_hop_arb` | keep | keep | flash-loan-aware (`multi_hop.rs:341`) |
| `jit` | keep | keep | pure fee capture; already inert on replay (see §5) |
| `Sandwich` | **remove** | **keep** | not flash-loan-aware; capital-intensive; needs builder access |
| `JitArb` | **remove** | **keep** | live copy has no honest P&L model |
| `Liquidation` | **disable wiring** | **keep** | detector models front-run capture without modelling the race |

`explorer/` is untouched throughout. See §2 for why.

## 1. Why the explorer stays

`Strategy` (opportunity evaluation) and `MevKind` (on-chain observation) are
independent enums with independent producers:

| | `Strategy` | `MevKind` |
|---|---|---|
| Sandwich | `types/strategy.rs:77` | `explorer/types.rs:16` |
| Liquidation | `types/strategy.rs:83` | `explorer/types.rs:23` |
| JitArb | `types/strategy.rs:78` | `explorer/types.rs:27` |

`MevKind::Sandwich` is produced by `classify_sandwiches` (`classify.rs:458`),
`MevKind::Liquidation` by the lending-pool pass (`classify.rs:68`). Neither
references any detector in `mev/detectors/`. Removing a `Strategy` variant
cannot affect them.

### JitArb in the explorer is structural, not optional

`classify.rs:336-356` is a kind **upgrade**, not a standalone pass:

```rust
let mut jit_events = classify_jit(input);
// jit_arb when a JIT tx also produced an arb in this block. The standalone
// ArbAtomic is suppressed so USD/P&L never double-counts the same flow.
let arb_txs: HashSet<u64> = /* ArbAtomic tx indices */;
let mut jit_arb_txs: HashSet<u64> = HashSet::new();
for ev in jit_events.iter_mut() {
    if arb_txs.contains(&ev.tx_index) {
        ev.kind = MevKind::JitArb;                                  // upgrade
        jit_arb_txs.insert(ev.tx_index);
    }
}
if !jit_arb_txs.is_empty() {
    events.retain(|e| !(e.kind == MevKind::ArbAtomic
                       && jit_arb_txs.contains(&e.tx_index)));     // suppress
}
```

Deleting `MevKind::JitArb` breaks the anti-double-count: the same flow would be
counted once as `Jit` and once as `ArbAtomic`, corrupting every USD/P&L figure
the explorer emits. Keeping the label but removing the `retain` leaves an
orphaned statement that describes nothing.

`explorer_corpus.rs:352` also holds a confirmed on-chain case
(`eth-jit-arb-26059522`). Removing it discards verified evidence.

**Action:** add a comment at `classify.rs:338` stating that this branch is load-bearing
and must survive removal of the live `JitArb` detector.

## 2. Why removal is safe against historical rows

`aggregate.rs:403`:

```rust
let strategy: Strategy = fill.strategy.parse().ok()?;
```

`.ok()?` drops unparseable strategy strings silently — there is no panic path.
`aggregate.rs:828` asserts this behaviour
(`"an unparseable strategy cannot be attributed to a bucket"`).

So historical `mev_ops.kind = 'sandwich' | 'jit_arb'` rows will vanish from
aggregate reports without error. Acceptable for historical sandbox data, but
update the doc comment at `aggregate.rs:387` to name the pruned variants
instead of describing a generic rule.

## 3. Phase 1 — Extract JIT tests (prerequisite)

`core/tests/sandwich.rs` also holds three JIT tests that must survive:

| Test | Line |
|---|---|
| `test_jit_detection_synthetic` | `sandwich.rs:119` |
| `test_jit_arb_detection_synthetic` | `sandwich.rs:321` |
| V3 real-pool JIT no-event check | `sandwich.rs:304-317` |

**Create `core/tests/jit.rs`** with those three plus the imports
(`JitDetector`, `JitArbDetector`, `JitDetector::new`).

Verify: `cargo test --test jit`

## 4. Phase 2 — Sandwich: full removal

| File | Change |
|---|---|
| `core/src/types/strategy.rs:77-78` | remove `Sandwich` variant |
| `core/src/types/strategy.rs:86-96` | remove from `Strategy::all()` |
| `core/src/mev/detectors/sandwich.rs` | delete file (~600 lines) |
| `core/src/mev/detectors/mod.rs` | remove `pub use` |
| `core/src/mev/mod.rs:1,9` | remove `SandwichDetector` export + module doc mention |
| `core/src/pipeline/runner.rs:13,436` | remove import + `SandwichDetector::new(block_num)` |
| `core/src/pipeline/aggregate.rs:85` | remove `Strategy::Sandwich => "sandwich"` arm |
| `core/src/pipeline/aggregate.rs:554,599-605,736-745,802` | rewrite 4 tests (see below) |
| `core/src/paper/ledger.rs:48` | remove from `is_native_eligible` |
| `core/src/paper/store.rs` | remove references |
| `core/src/jobs/live.rs` | remove references |
| `core/tests/sandwich.rs` | delete (after Phase 1) |
| `core/tests/replay.rs`, `e2e.rs` | remove references |

The four `aggregate.rs` tests deliberately exercise multi-strategy roll-up via
`by_strategy["sandwich"]`. Retarget them to `liquidation` or `jit` — deleting
them outright loses roll-up coverage.

Untouched: `explorer/` (including `explorer_corpus.rs` sandwich cases, which are
`MevKind`).

## 5. Phase 3 — JitArb: live removal

| File | Change |
|---|---|
| `core/src/types/strategy.rs:78-79` | remove `JitArb` variant |
| `core/src/types/strategy.rs:92` | remove from `all()` |
| `core/src/mev/detectors/jit_arb.rs` | delete file (~487 lines) |
| `core/src/mev/mod.rs:8` | remove `JitArbDetector` export |
| `core/src/pipeline/runner.rs:10` | remove import |
| `core/src/pipeline/runner.rs:122-126` | remove `with_proximity_window` builder |
| `core/src/pipeline/runner.rs:67,108` | remove `proximity_window` field |
| `core/src/pipeline/runner.rs:437-438,563-575` | remove detector instantiation, `process_tx`, `detect` |
| `core/src/pipeline/aggregate.rs:84` | remove `Strategy::JitArb => "jitarb"` arm |
| `core/src/paper/ledger.rs:47` | remove from `is_native_eligible` |
| `core/src/config/settings.rs:79-81,361,397` | remove `proximity_window` field + `default_proximity_window` |
| `core/src/config/settings.rs:944,1083` | remove `BacktestOverrides::proximity_window` + `(proximity_window, copy)` |
| `core/src/config/validation.rs:418-423,512-513` | remove the `> 100` cap and its test |
| `core/src/jobs/live.rs:305` | remove `.with_proximity_window(...)` |
| `core/src/jobs/run.rs:174` | remove `.with_proximity_window(...)` |
| `core/src/jobs/export.rs:60` | review `op.kind != "jit_arb"` condition |

⚠️ **Unrelated `Proximity`.** `explorer/decode.rs` has `LegSource::Proximity`
and a test `proximity_window_marks_leg_untrusted` (`:1543`). These are a
log-distance heuristic for token attribution — **unrelated** to the config
field. Do not touch them.

`explorer/` untouched.

### Known-inert

`core/tests/mev_corpus.rs:40-59` documents that `JitDetector` matches a mint
against swap logs, but replay through revm does not emit logs — so JIT produces
nothing on the live path today. `Jit` stays in this plan because it is inert
rather than wrong, and its removal is a separate decision.

## 6. Phase 4 — Liquidation: disable wiring

| File | Change |
|---|---|
| `core/src/pipeline/runner.rs:16` | remove `LiquidationDetector, AaveReserveCache` import |
| `core/src/pipeline/runner.rs:68,109` | remove `aave_reserve_cache` field + init |
| `core/src/pipeline/runner.rs:273-277` | remove `prefetch_aave_reserves` |
| `core/src/pipeline/runner.rs:440` | remove `LiquidationDetector::new(block_num)...` |
| `core/src/pipeline/runner.rs:563` area | remove the `process_tx`/`detect` call |
| `core/src/jobs/live.rs:311-315` | remove `prefetch_aave_reserves` call |
| `core/src/jobs/run.rs:180-182` | remove `prefetch_aave_reserves` call |
| `core/src/mev/detectors/liquidation.rs` | delete file |
| `core/src/mev/mod.rs:8-9` | remove `LiquidationDetector`, `AaveReserveCache`, `AaveReserveData` exports |
| Aave V3 reserve types | remove if unreferenced after the above (grep first) |

`Strategy::Liquidation` **stays in the enum** (without a detector) so the DB
`kind` column and historical reports keep parsing. Remove it from
`Strategy::all()`.

**Note on `jobs/run.rs`.** `job_run` has no CLI subcommand and no production
caller — it is reachable only from tests (`paper_corpus.rs:56,274`,
`mev_corpus.rs:155,586,691`). It must still compile and its tests must pass.

### What the liquidation detector actually does

The detector sees an already-executed `LiquidationCall` in a replayed block and
models taking over that liquidation. That is front-run capture, and it is
genuinely flash-loan-executable. What it does **not** model is the race: another
searcher will usually win the same opportunity. `runner.rs:26-34` has a
confidence-decay heuristic for persistence, but without builder/relay access
this strategy will consistently lose. Hence disable rather than delete-from-
`all()`.

## 7. Phase 5 — Documentation

| File | Lines |
|---|---|
| `docs/mev_strategies.md` | `2570`, `2607`, `3298`, `3425`, `§3.3` (~`573-601`), `§44` (~`4949`), `2607` combo table, `3719-3720`, `3893-3894` |
| `docs/ARCHITECTURE.md` | `350` (detector list), `620`, `645` |
| `docs/roadmap_to_100pct.md` | `95` (status table), `286-298` |
| `mev-scout.example.toml` | `27` (`strategies = "all"`) |
| `core/src/config/settings.rs` | `69` (doc example mentions `sandwich`) |
| `core/src/mev/mod.rs` | `1` (module doc) |
| `core/src/explorer/classify.rs` | `338` (add load-bearing comment) |

Update the capital-free inventory (`mev_strategies.md:2693-2710`) to note that
liquidation is observation-only in the current tree.

## 8. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| `AaveReserveCache` / `AaveReserveData` referenced elsewhere | High | grep before deleting; only `LiquidationDetector` + runner use them |
| `proximity_window` still referenced in CLI or config | Medium | covered in Phase 3; check `cli/src` |
| Multi-strategy roll-up coverage lost from 4 `aggregate.rs` tests | Medium | retarget to `liquidation` / `jit`, do not delete |
| `explorer_corpus.rs` / golden tests break | Low | `MevKind` untouched; confirm with `cargo test` |
| `job_run` compile break | Low | it is test-reachable, so `cargo test` catches it |
| Historical rows silently dropped from aggregate | Low | update `aggregate.rs:387` doc comment |

## 9. Execution order

Each phase is a separate commit.

```
Phase 1  → cargo test --test jit          (new file passes)
Phase 2  → cargo test                     (corpus/golden green)
Phase 3  → cargo test
Phase 4  → cargo test
Phase 5  → cargo clippy --all-targets -- -D warnings
```

## 10. Known pre-existing gap (out of scope)

`[backtest] strategies` is inert. The only production read is
`jobs/live.rs:287`, and it merely decides whether to run `init_pools`. No
detector tests membership. The spec'd `--strategies` CLI flag and `[detectors]`
TOML section do not exist (`cli/src/cli.rs:26-52`).

Consequence for this plan: pruning strategies is a code change, not a config
change. `strategies = "two_hop_arb,multi_hop_arb,jit"` will not restrict
detection. Decide separately whether to implement the filter.