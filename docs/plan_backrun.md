# Plan — Backrun detection (`Strategy::Backrun`)

**Status:** partially implemented (uncommitted working tree) — **§10 remediation must
close 7 gaps before this plan is done**; §9 checklist reflects only what is verified
**Spec:** `docs/mev_strategies.md` §41 (backrun detection), §2.1 (backrunning)

---

## 0. Locked decisions

| # | Decision | Choice |
|---|---|---|
| D1 | **Scope** | Historical only — `run_block` (REVM) **and** `sync_block_from_logs` (log-only). No pending-tx simulation (`docs/mev_strategies.md` §41.2) in this plan. |
| D2 | **Precedence** | A backrun claim **supersedes** the plain `two_hop_arb` / `multi_hop_arb` claim for the same key in the same block. Mirrors `explorer::classify` (`core/src/explorer/classify.rs:395–406`, *"so P&L is never double-counted"*). |
| D3 | **Causal bar** | **Net flip**: `net(pre) ≤ 0 ∧ net(post) > 0`, where `net(x) = expected_profit(x) − gas_cost_wei`. |

### Why D3 rather than a threshold

`docs/mev_strategies.md:4828–4838` requires comparing the opportunity before and after the
target transaction, and explicitly forbids classifying on adjacency alone. The net-flip rule
is the exact executable form of *"the opportunity exists primarily because of A"*:

* `net(pre) > 0` → the gap was already executable before `A`, so it is a plain arb, not a backrun.
* `net(post) ≤ 0` → `A` did not create an executable opportunity.

No tunable threshold means no per-chain tuning and no recall/precision dial to re-litigate.

---

## 1. Grounding — what exists today

| Piece | State | Reference |
|---|---|---|
| `Strategy::TwoHopArb` / `MultiHopArb` / `Jit` | implemented | `core/src/mev/detectors/`, `core/src/types/strategy.rs:71–78` |
| `Strategy::Backrun` | **absent** | `Strategy::all()` = 3 entries (`strategy.rs:92–94`) |
| `core/src/mev/detectors/backrun.rs` | **absent** | `core/src/mev/detectors/mod.rs` exports only `jit`, `mempool`, `multi_hop`, `two_hop` |
| `MevOpportunity.victim_tx_index` / `backrun_tx_index` | exist, always `None` | `core/src/types/opportunity.rs:71–79`; set to `None` at `arb_common.rs:181`, `jit.rs:371` |
| Explorer realized-backrun classifier | implemented (logs-only proxy) | `core/src/explorer/classify.rs:905` `classify_backruns` |
| Explorer `MevKind::Backrun` | implemented | `core/src/explorer/types.rs:21` |
| Golden causal set (backrun/frontrun ground truth) | implemented | `core/src/explorer/golden.rs:171` `causal_labeled_set`, `:506` `score_embedded_causal_set` |
| `Strategy` config list | **inert** — no detector reads it | `mev-scout.example.toml:27–29` |

### The load-bearing observation

Both runner paths already run **detect-before-apply** on every transaction:

```text
run_block                pipeline/runner.rs:432–530
sync_block_from_logs     pipeline/runner.rs:693–717

for i in txs:
    detect(S_{i-1})          ← pm before update_from_logs
    update_from_logs(tx_i)   → S_i
    take_dirty_pools()
```

So the pipeline *already* computes the arbitrage opportunities on `S_i` — the post-victim
state — at iteration `i+1`. What is missing is entirely on the claim side:

1. the matching **pre-image** on `S_{i-1}` for the same path,
2. the **flip test** between them,
3. the **attribution** (`victim_tx_index`) and its **canonical id**,
4. the **suppression** of the duplicate plain-arb row.

That is the whole feature. No new quote engine, no new math, no REVM work.

---

## 2. Design

### 2.1 State timeline

```text
S_{i-1}  --update_from_logs(tx_i)-->  S_i
   ↑                                     ↑
   pre-image pass                        post-image pass
   scope = Dirty(will_touch)             scope = Dirty(newly_dirty)
```

* `will_touch = { log.address : log ∈ tx.logs } ∩ pool_addresses` — computed **before** the
  update from logs already available in the closure (`tx.logs` in `run_block`,
  `logs` in `sync_block_from_logs`).
* `newly_dirty = take_dirty_pools()` — pools whose state `A` actually changed.

`will_touch ⊇ newly_dirty` always holds (only logged pools can be dirtied), so every path
whose state changed by `A` is in **both** scopes. That gives:

* **Correctness** — a path that did not touch any pool `A` touched cannot have changed state,
  so `net(pre)` and `net(post)` are equal for it and D3 cannot fire. Pool overlap (the
  explorer's `STATE_DELTA_MATCH` evidence, `classify.rs:1000`) is therefore *implied* by
  construction, not separately enforced.
* **A free gate** — if `newly_dirty.is_empty()` (reverted tx, or a tx whose logs did not move
  any tracked pool), **skip the post pass entirely**. Zero cost, no recall loss: `A` changed
  nothing, so `A` cannot have created anything.

### 2.2 Why the pre-image needs its own detector instances

The pre-image must be a *faithful snapshot* of `S_{i-1}`, but opportunities flowing out of the
long-lived `two_hop_detector` / `multi_hop_detector` are already **deduped** (`dedup_arb`,
`arb_common.rs:108`) and **scope-filtered**. Building the pre-image from
`all_opportunities` would silently read "absent before" for a path that was merely deduped —
producing false backruns.

Therefore `BackrunDetector` owns **four fresh detector instances per tx**:

```rust
struct BackrunDetector {
    block_number: u64,
    pre_two:   TwoHopArbDetector,    // fresh each tx → no cross-tx dedup contamination
    pre_multi: MultiHopArbDetector,
    post_two:  TwoHopArbDetector,    // fresh each tx
    post_multi: MultiHopArbDetector,
    seen: HashMap<(Address, Address, Address, Address), (u128, u128)>, // cross-family, cross-tx
}
```

Construction is cheap (`HashMap::new`). The expensive part is the multi-hop graph build, which
is gated by §2.1.

### 2.3 Differential

```text
pre  = pre_two.detect(S_{i-1}, Dirty(will_touch))  ∪  pre_multi.detect(S_{i-1}, Dirty(will_touch))
post = post_two.detect(S_i,    Dirty(newly_dirty)) ∪ post_multi.detect(S_i,    Dirty(newly_dirty))

pre_index: key(opp) → expected_profit          // from `pre`

for o in post:
    p0  = pre_index.get(key(o)).copied().unwrap_or(U256::ZERO)   // absent ⇒ unchanged-but-undetected ⇒ 0
    net0 = p0        − U256::from(o.gas_cost_wei)
    net1 = o.expected_profit − U256::from(o.gas_cost_wei)
    if net1 > 0 && net0 <= 0 && cross_family_dedup(o):
        emit o as Strategy::Backrun { victim_tx_index: i, ... }
```

`key(o)` = `(min(pool_a,pool_b), max(pool_a,pool_b), token_in, token_out)` — orientation
normalised, because two-hop and multi-hop can report the same two-pool cycle with opposite
`pool_a`/`pool_b` (two_hop: `buy_pool`/`sell_pool`, `two_hop.rs:172–173`; multi_hop:
`path[0]`/`path[last]`, `multi_hop.rs:365–366`).

Cross-family dedup uses `check_dedup_key` (`pool::state::manager.rs:824`, already `pub`) with
that normalised key, so it stays reserve-aware: a gap that closes and reopens under a
different victim re-emits, while the same open gap does not.

`unwrap_or(ZERO)` on a missing pre-image is deliberate: `will_touch ⊇ newly_dirty`, so any
post candidate was in scope pre-update; a miss means the path was genuinely not detected on
`S_{i-1}`. The only residual way to miss is a sub-0.1%-liquidity state change that the
*pre* pass deduped — impossible, because pre detectors are fresh each tx.

### 2.4 Precedence (D2)

Block-level, after the tx loop, **before** `retain_with_rejections` — so superseded rows are
not mis-recorded as gas/min-profit rejections:

```rust
let backrun_keys: HashSet<Key> = backruns.iter().map(key).collect();
all_opportunities.retain(|o| {
    !(matches!(o.strategy, Strategy::TwoHopArb | Strategy::MultiHopArb)
        && backrun_keys.contains(&key(o)))
});
```

Mirrors `classify.rs:399–406` (`superseder` + `events.retain`).

Why block-level and not `tx_index`-matched (the explorer keys its superseder on `tx_index`):
the explorer processes *realized* transactions, so one `tx_index` carries exactly one claim.
Here the plain arb for a post-victim gap surfaces at `tx_index = victim + 1` while a
pre-existing gap surfaced at its own index, and the two are only distinguishable by path. The
key is the path.

Residual risk: two distinct multi-hop cycles sharing the same first/last pool and endpoint
tokens would over-suppress. Rare; documented, covered by a test that pins the intended
behaviour.

### 2.5 Canonical id

`compute_canonical_id` (`opportunity.rs:115`) is `Strategy|pool_a|pool_b|token_in|token_out` —
it carries no victim, so two victims in one block would collide under `aggregate`'s
canonical-id dedup (`pipeline/aggregate.rs:167–171`).

Add a dedicated builder that **matches the explorer's form exactly**
(`explorer/canonical.rs:46–55`):

```rust
// core/src/types/opportunity.rs
pub fn compute_backrun_canonical_id(anchor_pool: Address, victim_tx_index: usize) -> String {
    format!("Backrun|{anchor_pool:#x}|source_tx:{victim_tx_index}")
}
```

where `anchor_pool` = first path pool present in `newly_dirty`. This is the one place where
T1 exact matching (`validate.rs:5`, `explorer/canonical.rs:14–16` marks T1 aspirational) can
actually land, because the explorer's realized id is built from its single anchor pool
(`classify.rs:975`, `pools: vec![pool]`).

T2 (≥1 pool in common + same token direction + block window) is the workhorse and is
structurally guaranteed: `anchor_pool ∈ path`, so the opportunity's pool set always overlaps
the realized event's `pools: vec![anchor_pool]`.

---

## 3. File-by-file changes

### 3.1 New

| File | Contents |
|---|---|
| `core/src/mev/detectors/backrun.rs` | `BackrunDetector` — `pre_detect`, `post_detect`, normalised `key`, cross-family `seen` |
| `core/tests/backrun.rs` | Unit + integration coverage (§6) |

### 3.2 `core/src/types/strategy.rs`

```rust
#[strum(serialize = "backrun")]
Backrun,
```

* `Strategy::all()` (line 92): append `Strategy::Backrun` — this is both the `"all"` expansion
  and `default_strategies()` (`settings.rs:303`).
* `RETIRED_STRATEGY_NAMES` (line 86): unchanged.
* `from_comma_list` needs no change (parses through strum).

### 3.3 `core/src/types/opportunity.rs`

* Add `compute_backrun_canonical_id` (§2.5).
* Update the doc comments on `victim_tx_index` / `backrun_tx_index` (lines 71–79), which
  currently say *"Always `None` in the current tree"*.

### 3.4 `core/src/mev/detectors/mod.rs`

```rust
pub mod backrun;
pub use backrun::BackrunDetector;
```

### 3.5 `core/src/pipeline/runner.rs` — `run_block`

Inside the `on_tx` closure:

```rust
let mut pm = pool_manager.borrow_mut();

// (A) NEW — pre-image, before any state change
let will_touch: HashSet<Address> = tx.logs.iter()
    .map(|l| l.address)
    .filter(|a| pool_addrs.contains(a))
    .collect();
if !will_touch.is_empty() {
    let scope_pre = ScanScope::Dirty(&will_touch);
    backrun.pre_detect(&pm, i, timestamp, base_fee_per_gas, self.gas_config, &scope_pre);
}

// (B) existing two_hop / multi_hop / jit detection — UNCHANGED
let dirty_snapshot = dirty_pools.borrow().clone();
...

// (C) existing state application — UNCHANGED
pm.learn_taxes_from_tx(&tx.logs);
pm.update_from_logs(&tx.logs);
let newly_dirty = pm.take_dirty_pools();

// (D) NEW — post-image + differential + emission
if !newly_dirty.is_empty() {
    let scope_post = ScanScope::Dirty(&newly_dirty);
    let opps = backrun.post_detect(&pm, i, timestamp, base_fee_per_gas,
                                   self.gas_config, &scope_post, &tx.logs, &txs);
    all_opportunities.extend(opps);
}

// (E) existing dirty_pools accumulation — UNCHANGED
if !newly_dirty.is_empty() { dirty_pools...extend(newly_dirty); }
```

`backrun_detector` is constructed next to `two_hop_detector` / `multi_hop_detector`
(line 390). `post_detect` needs `&txs` to stamp `victim_tx_index` and leave
`sender` / `tx_hash` pointing at the **anchor (victim)** transaction (§4).

After the loop, **before** `retain_with_rejections` (line 553):

```rust
// (F) NEW — D2 precedence
suppress_superseded_arbs(&mut all_opportunities, &backrun);
```

Identical insertion in `sync_block_from_logs`: pre at line ~693, post between
line 717 (`update_from_logs`) and 718 (`take_dirty_pools`) — note the existing code calls
`take_dirty_pools()` inline at 718; hoist it into a binding so `newly_dirty` is available to
both the backrun pass and the existing `dirty_pools` extend.

### 3.6 Exhaustive / behaviour-sensitive matches

These break at compile time (good) or fail silently (bad) when the variant is added:

| File:line | Change | Failure mode if missed |
|---|---|---|
| `core/src/pipeline/aggregate.rs:81–85` `ui_strategy_name` | add `Strategy::Backrun => "backrun"` | **compile error** |
| `core/tests/mev_corpus.rs:234–239` `strategy_kind` | add `Strategy::Backrun => Some("backrun")` | **compile error** |
| `core/src/paper/ledger.rs:41–46` `is_native_eligible` | add `Strategy::Backrun` | **silent** — paper fills drop every backrun (`matches!` is non-exhaustive) |
| `core/src/paper/ledger.rs:347` test | `Strategy::all().len()` 3 → 4 | test failure |
| `core/tests/mev_corpus.rs:248` `ALL_KINDS` | `&["arb_atomic", "jit", "backrun"]` | record runs never print a `backrun` line |
| `core/tests/mev_corpus.rs:797–805` `every_detector_kind_is_reported_by_a_record_run` | add `"backrun"` to the list, `len()` 2 → 3 | test failure |
| `core/src/explorer/validate.rs:45–54` `strategy_to_kind` | add `"backrun" => Some("backrun")` | **silent** — scanner backruns never join realized backruns, recall metric stays 0 |

`strategy_to_kind` carries an explicit comment (*"scanner strategies do **not** map to
`backrun`/`frontrun`"*, `validate.rs:22–28`) that must be rewritten: backrun becomes the first
scanner strategy joinable to the realized `MevKind::Backrun`. Per `validate.rs:27`, unmapped
strategies currently contribute only to `precision_signal_count` — the mapping moves backrun
into the recall denominator, which is the point.

### 3.7 Config / docs

* `mev-scout.example.toml:26–30` — update the `"all"` expansion comment to include `backrun`.
  Keep the *"currently inert — no detector tests this list"* note: **D4, this plan does not
  introduce config gating.** Making `strategies` live for backrun alone would make it the
  first gated detector and would contradict the *inert* semantics documented in
  `mev-scout.example.toml:27–29`. Revisit only if
  §5 performance measurement says the pass is too expensive.
* `core/src/config/settings.rs:69` doc comment example.
* `docs/mev_strategies.md:2873` — flip `Backrunning` row from `Planned (backrun.rs)` to coded.
* `docs/mev_strategies.md:3920–3926` — extend the "Implemented subset" sentence.
* `docs/CLI.md:521–536` — add a line contrasting the realized logs-only proxy with
  the scanner's state-differential (this is the first `profit(B|before)` vs
  `profit(B|after)` computation in the tree).

---

## 4. Data contract for `Strategy::Backrun`

| Field | Value | Rationale |
|---|---|---|
| `block_number` | block under scan | — |
| `tx_index` | `i` (victim index) | keeps the mechanical rule `sender = txs[tx_index].from` intact |
| `victim_tx_index` | `Some(i)` | the semantic field the sandwich code path already reads (`classify.rs:386`, `canonical.rs:31`) |
| `backrun_tx_index` | `None` | no realized backrun tx exists for a *candidate*; asserting `i+1` would name an unrelated tx |
| `sender` | `txs[i].from` (stamped by the runner) | documented as the **anchor** sender, not a searcher |
| `tx_hash` | `txs[i].hash` | matches the field doc *"anchored to a specific transaction"* (`opportunity.rs:92–95`) |
| `pool_a` / `pool_b` / `path` | as emitted by the underlying detector | unchanged semantics |
| `canonical_id` | `compute_backrun_canonical_id(anchor_pool, i)` | §2.5 |
| `detection_path` | `REPLAY_PATH` or `LOG_ONLY_PATH` | set by the existing stamping pass |
| `expected_profit` | `net(post)` side — the post-image profit | `retain_with_rejections` then applies the same gas/min-profit gate as every other strategy |

Add a short note to `MevOpportunity`'s doc header (`opportunity.rs:10–14`) that for
`Strategy::Backrun`, `sender`/`tx_hash`/`tx_index` denote the **anchor** transaction.

---

## 5. Performance

Cost per transaction **with** a tracked-pool log:

| Pass | Before | After |
|---|---|---|
| two-hop | 1 | 2 (pre-image adds 1) |
| multi-hop | 1 | 2 |
| JIT | 1 | 1 |

and **zero** extra passes for a tx that dirties no pool (§2.1 gate).

The real cost driver is multi-hop: `MultiHopArbDetector::find_negative_cycle_paths`
(`multi_hop.rs:120–157`) builds the whole `TokenGraph` and runs up to
`MAX_CYCLE_ATTEMPTS = 64` Bellman–Ford rounds **before** the `ScanScope` filter is applied, so
a `Dirty` scope narrows the results but not the graph work.

Mitigations, in order:

1. The `newly_dirty.is_empty()` gate (free, removes reverted / non-pool txs).
2. `will_touch.is_empty()` gate (free).
3. Measure first. Acceptance criterion: full backtest over the corpus window shows ≤ 20%
   wall-clock regression vs `main`.
4. Only if (3) fails: raise a dedicated anchor threshold (e.g. skip unless some touched pool's
   liquidity estimate moved ≥ N bps). This is a recall dial and must be documented as such —
   **not** introduced speculatively.

**Measurement (2026-10-07):** not run — no RPC baseline.

---

## 6. Testing

### 6.1 `core/tests/backrun.rs` (new)

Use the synthetic-pool harness in `core/tests/common/setup.rs` (already drives both arb
detectors, lines 150/160).

| Test | Asserts |
|---|---|
| `flips_from_unprofitable_to_profitable` | pre unprofitable, apply victim swap log, post profitable → one `Strategy::Backrun` with `victim_tx_index = Some(v)` |
| `no_claim_when_gap_predates_victim` | gap already executable on `S_{i-1}` → **no** backrun (D3 `net(pre) ≤ 0` fails) |
| `no_claim_when_state_unchanged` | victim tx dirties no pool → `post_detect` never called |
| `no_claim_when_still_unprofitable_after` | `A` moves the pool but `net(post) ≤ 0` → no backrun |
| `no_claim_when_path_never_touched_by_victim` | `will_touch ∩ path = ∅` → excluded by scope, no backrun |
| `dedups_same_gap_across_both_families` | two-hop and multi-hop both detect the post-image → exactly **one** backrun row |
| `supersedes_plain_arb_in_same_block` | a `two_hop_arb` row for the same key is dropped; a pre-existing unrelated arb in the same block is **retained** |
| `emits_once_per_block_when_gap_persists` | multiple later txs leave the gap open → one row |
| `re_emits_after_gap_closes_and_reopens` | `check_dedup_key` reserve change → second victim gets its own row |
| `canonical_id_matches_explorer_form` | id equals `Backrun\|{anchor}\|source_tx:{v}` |

### 6.2 Existing suites

* `core/tests/arbitrage.rs` — must stay green (two/multi-hop behaviour is unchanged by design).
* `core/tests/replay.rs` — line 248 `matches!(o.strategy, TwoHopArb | MultiHopArb)`: confirm
  whether the test needs `| Backrun`, driven by what the fixture actually produces.
* `core/tests/paper_corpus.rs` — line 27 documents the emittable set as
  `TwoHopArb / MultiHopArb / Jit`; update.
* `core/tests/mev_corpus.rs` — §3.6 changes; `MEV_SCOUT_RECORD=1` run records a `backrun`
  line (explicit `0 ops` is an acceptable, informative result and is exactly what
  `ALL_KINDS` exists for).
* `core/src/paper/ledger.rs` — `every_runnable_strategy_is_native_eligible` must still pass.

### 6.3 Validation against ground truth (the deliverable that matters)

```bash
cargo test -p mev-scout-core golden
cargo test -p mev-scout-core --test explorer_golden
```

`explorer::golden::score_embedded_causal_set` (`golden.rs:506`) is the labelled backrun /
frontrun set referenced as the Phase 3 ship gate in `docs/CLI.md:514–516`.
Once `strategy_to_kind` maps `backrun`, the scanner's backrun rows can be scored against it
via `validate` — **the first scanner-level backrun precision/recall number in the project.**

Record the before/after in this file when the first run completes.

**First run (2026-10-07):** `cargo test -p mev-scout-core golden` and
`cargo test -p mev-scout-core --test explorer_golden` both green (embedded labelled set;
2 + 2 tests). Scanner-level precision/recall via `validate`: not run — no RPC baseline.

### 6.4 Commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 7. Out of scope (follow-ups, not this plan)

1. **Live pending backrun (§41.2)** — simulate a pending tx `A` against pending state to
   produce `S'`, then run the detectors on `S'`. Requires revm application of an unmined tx
   against pending state; separate plan. `docs/ROADMAP_LIVE_BOT.md:325` already records that
   backrun is unviable on the *public* mempool, so this is coupled to L3.4 private channels.
2. **Arbitrage-trigger / automation backrun** (`docs/mev_strategies.md` §21).
3. **Config gating of detectors** — the `strategies` list stays inert (§3.7).
4. **`FRONTRUN` scanner strategy** — same differential, opposite ordering; not attempted.
5. **REVM counterfactual for the realized explorer backrun** — the explorer's
   `classify_backruns` stays logs-only (`CLI.md:521`). This plan adds the scanner-side
   differential only; wiring the explorer to consume it is a separate change.

---

## 8. Risks

| Risk | Mitigation |
|---|---|
| Extra multi-hop graph build per pool-touching tx blows the budget | §5 measurement gate before anything else; anchor threshold only if measured |
| Orientation mismatch between two-hop and multi-hop leaves a duplicate arb row | normalised key (§2.3, §2.4) + explicit test |
| Over-suppression of a legitimate pre-existing arb (shared endpoints, different middle pools) | documented; keyed suppression limited to the same block; test pins the retained case |
| Mapping `backrun` in `strategy_to_kind` moves it into the recall denominator and could *lower* headline recall if candidates are noisy | golden-set scoring (§6.3) runs before the change is called done |
| `sender`/`tx_hash` semantics shift to "anchor" for this strategy and leak into searcher metrics | documented in §4 and in the struct doc; T2 matching does not use sender (`validate.rs:642` is report-only) |
| Config `strategies = "all"` now lists `backrun` while the list remains inert | comment kept verbatim in `mev-scout.example.toml` |

---

## 9. Execution checklist

**Core (audit 2026-10-06):**

- [x] `Strategy::Backrun` + `all()` + `compute_backrun_canonical_id` (§3.2, §3.3)
- [x] Fix the three compile-breaking matches (§3.6) — build passes
- [x] Fix `is_native_eligible` + `Strategy::all().len()` 4 (§3.6)
- [x] `core/src/mev/detectors/backrun.rs` + `mod.rs` export (§3.1)
- [x] Wire `run_block` (§3.5 A–E) → §10 G1/G3 closed
- [x] Wire `sync_block_from_logs` (§3.5) → §10 G4 closed
- [x] `strategy_to_kind` + `ALL_KINDS` + `strategy_kind` + count assertions (§3.6)
- [x] `core/tests/backrun.rs` (§6.1) → §10 G5 closed — 10/10 green
- [x] Update `paper_corpus` / `replay` / doc expectations (§6.2) → §10 G6
- [x] Config + doc updates (§3.7) → §10 G6
- [x] `cargo fmt --all -- --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace` (25 binaries, 0 failures; `golden` + `explorer_golden` green)
- [ ] Performance measurement against the corpus window (§5.3) — not run — no RPC baseline
- [ ] Golden-set backrun precision/recall (§6.3) — not run — no RPC baseline (embedded `golden`/`explorer_golden` tests pass)

**Remediation (§10):** see the G1–G7 checklist at the end of §10.

---

## 10. Remediation — gap audit and closure plan

Audit of the uncommitted implementation against this plan (2026-10-06). Seven gaps;
G1–G4 are correctness, G5–G7 are coverage/docs/gates.

### G1 — pre-image is computed on the wrong state (critical)

**Finding.** `run_block` calls `backrun_detector.pre_detect(...)` at
`runner.rs:540` but **discards the result** (`let _ =`). `post_detect`
(`backrun.rs:137–144`) then re-runs `pre_detect` internally against the
**post-victim** `pool_manager` with the **post** scope. So `net(pre)` is measured on
`S_i`, not `S_{i-1}`:

* whenever both passes find the same gap, `net0 == net1 > 0` and D3 never fires;
* when the pre detectors' own `seen` dedup suppresses the re-detection, `pre_index`
  misses the key, `net0` falls back to `0 ≤ 0`, and **every** net-positive post
  opportunity becomes a "backrun" — i.e. the rule degenerates to `net(post) > 0`
  and `no_claim_when_gap_predates_victim` fails.

A second, independent defect: the four detector instances are constructed **once per
block** (`BackrunDetector::new` at `runner.rs:393`), not "fresh each tx" as §2.2
requires. Their persistent `seen` maps make a pre-existing gap read as *absent before*
once an earlier tx's pre pass has already recorded it.

**Fix.**

1. `BackrunDetector` gains `pre_image: Vec<MevOpportunity>` + `pre_image_tx: usize`.
2. `pre_detect` stores its output (`self.pre_image = opps; self.pre_image_tx = tx_index;`)
   and returns it (runner may keep or drop the return value).
3. `pre_detect` **resets** `pre_two`/`pre_multi` at entry, `post_detect` **resets**
   `post_two`/`post_multi` at entry — fresh per tx per §2.2. Construction is two
   `HashMap::new()`s (cheap by design; §2.2). The only persistent state is
   `self.seen` (cross-family, cross-tx emission dedup) and `pre_image`.
4. `post_detect` builds `pre_index` from `self.pre_image` **only** (no re-detection),
   guarded by `self.pre_image_tx == tx_index`. On mismatch treat as empty — a state
   that is unreachable (a non-empty `newly_dirty` ⊆ `will_touch` guarantees the runner
   called `pre_detect` for this tx) and therefore safe by construction.
5. Runner: keep the `will_touch.is_empty()` gate (§2.1); the `let _ =` becomes
   meaningful because the detector now retains the value.

### G2 — `canonical_id` is clobbered after emission

**Finding.** `post_detect` sets `compute_backrun_canonical_id` (`backrun.rs:179`), but
the unconditional stamping loops at `runner.rs:607–616` (`run_block`) and
`runner.rs:799–808` (`sync_block_from_logs`) overwrite **every** row with the generic
`compute_canonical_id`. Two victims in one block therefore collide under
`aggregate.rs`'s canonical-id dedup — exactly the risk §2.5 exists to prevent.

**Fix.** In both stamping loops, skip the generic rewrite for `Strategy::Backrun`
(`if opp.strategy != Strategy::Backrun { ... }`). The backrun id stays whatever
`post_detect` stamped. `detection_path` and `sender`/`tx_hash` backfill keep running
unchanged for backrun rows.

**Anchor rule.** §2.5 says anchor = first path pool present in `newly_dirty`.
Implement in `post_detect`, where the `ScanScope` is in hand: walk
`o.path` (fallback `pool_a`/`pool_b`) and take the first pool contained in the
`Dirty` set; fall back to `pool_a.min(pool_b)` for `ScanScope::Full`. Compute once,
at emission — the stamping loop no longer touches it.

### G3 — D2 precedence (§2.4) is not implemented at all

**Finding.** No `suppress_superseded_arbs` / `backrun_keys` anywhere in
`core/src/pipeline/`. Duplicate `two_hop_arb`/`multi_hop_arb` rows for a path already
claimed as `Strategy::Backrun` survive into persistence — §2.4's "P&L is never
double-counted" invariant is violated.

**Fix.** New free function in `backrun.rs`:

```rust
pub fn suppress_superseded_arbs(opps: &mut Vec<MevOpportunity>)
```

derives the key set from the `Strategy::Backrun` rows **already inside `opps`**
(no extra threading needed), then `retain`s per §2.4. `BackrunKey::from_opp` becomes
`pub`. Call sites, both **before** `retain_with_rejections` so superseded rows are
never recorded as gas/min-profit rejections:

* `run_block` — `runner.rs:591`, before line 592.
* `sync_block_from_logs` — before its `retain_with_rejections` (`runner.rs:785`).

### G4 — `sync_block_from_logs` is not wired

**Finding.** `runner.rs:698–782` has no `backrun_detector` at all; the log-only path
never emits a backrun, so `detection_path = "log_only"` backrun coverage is zero and
§3.5's "identical insertion" was skipped.

**Fix** (mirrors `run_block`, respecting the existing detect-before-apply order):

1. `let mut backrun_detector = BackrunDetector::new(block_num);` next to the other
   detectors (`runner.rs:688`).
2. **Pre pass** — before `learn_taxes_from_tx`/`update_from_logs` (`:755`): build
   `will_touch` from `logs` ∩ `pool_addrs`; if non-empty, `pre_detect(..., Dirty(will_touch))`.
3. **Post pass** — after `take_dirty_pools()` (`:757`) and **before** the
   `dirty_pools.extend(newly_dirty)` at `:758–762` (the set is consumed there):
   if `!newly_dirty.is_empty()`, `post_detect(..., Dirty(&newly_dirty), &logs, &txs)`
   and extend `all_opportunities`. `newly_dirty` is already hoisted into a binding —
   the hoist §3.5 asked for is done.
4. **Suppression** — `suppress_superseded_arbs` before `retain_with_rejections` (G3).
5. `LOG_ONLY_PATH` stamping at `:809` already runs for every row; combined with G2's
   skip it stamps `detection_path` on backrun rows without touching `canonical_id`.

### G5 — `core/tests/backrun.rs` does not exist

All ten §6.1 tests are missing; nothing pins D3, the scope gates, the cross-family
dedup, D2, or the canonical-id form. New file, using the §6.1 harness plus:

* pool/log construction modelled on `manager.rs:894–905` (`ExecutedLog` with
  `SWAP_TOPIC`, `B256::ZERO` topics, 4-word V2 swap data) and `core/tests/common/setup.rs`
  (`make_pool`, `synthetic_arb_pools`, `make_synthetic_runner`);
* detector-level tests call `BackrunDetector::{pre_detect, post_detect}` directly with
  an explicit `ScanScope`;
* runner-level tests (`no_claim_when_state_unchanged`, and a canonical-id
  non-clobber assertion) go through `make_synthetic_runner`/`run_block`.

| Test | Pin |
|---|---|
| `flips_from_unprofitable_to_profitable` | D3 fires; `victim_tx_index = Some(v)` |
| `no_claim_when_gap_predates_victim` | **the G1 regression test** — pre-image must come from `S_{i-1}` |
| `no_claim_when_state_unchanged` | empty `newly_dirty` ⇒ post pass skipped |
| `no_claim_when_still_unprofitable_after` | `net(post) ≤ 0` ⇒ no claim |
| `no_claim_when_path_never_touched_by_victim` | `Dirty` scope excludes an untouched path |
| `dedups_same_gap_across_both_families` | exactly one row when two-hop + multi-hop both see it |
| `supersedes_plain_arb_in_same_block` | unit test of `suppress_superseded_arbs`: same-key arb dropped, unrelated arb retained, backrun row retained |
| `emits_once_per_block_when_gap_persists` | `self.seen` blocks a second emission |
| `re_emits_after_gap_closes_and_reopens` | >0.1% reserve move ⇒ second victim re-emits |
| `canonical_id_matches_explorer_form` | `Backrun\|{anchor}\|source_tx:{v}` **after a full `run_block`** (pins G2) |

### G6 — docs, config and stale comments

| # | File | Change |
|---|---|---|
| 1 | `mev-scout.example.toml:26–30` | `"all" resolves to two_hop_arb, multi_hop_arb, jit, backrun`; keep the *inert* note (D4 unchanged) |
| 2 | `docs/mev_strategies.md:2873` | row 11 `Backrunning` → mark **coded** (`core/src/mev/detectors/backrun.rs`) |
| 3 | `docs/mev_strategies.md:3729` | status table: `Planned (backrun.rs)` → `Implemented (scanner, historical)` |
| 4 | `docs/mev_strategies.md:3920–3926` | extend the "Implemented subset" sentence with backrun (scanner-side state-differential, historical only; §41.2 live still out of scope) |
| 5 | `docs/CLI.md:521–536` | add the contrast line: realized backrun stays logs-only proxy, while the scanner now computes the tree's first `profit(B\|before)` vs `profit(B\|after)` differential (`plan_backrun.md` D3) |
| 6 | `core/src/explorer/validate.rs:22–28` | rewrite the stale comment: `backrun` **is** now mapped (`:52`) and is the first scanner strategy joinable to realized `MevKind::Backrun`; `frontrun` remains unmapped (no scanner strategy) |
| 7 | `core/tests/paper_corpus.rs:27` | `is_native_eligible` list now includes `Backrun` |
| 8 | `core/tests/replay.rs:248` | **no change** — the synthetic fixture has empty receipt logs (`setup.rs:252`), so `will_touch` is always empty and no backrun is emitted; record the decision here so it is not re-litigated |
| 9 | `core/src/types/opportunity.rs:71–78` | fix the mis-indented doc comments; add the §4 note to the struct doc header (`:10–14`) that `sender`/`tx_hash`/`tx_index` are the **anchor** transaction for `Strategy::Backrun` |
| 10 | `core/src/config/settings.rs:69` | no change needed — the doc example (`"two_hop_arb,jit"`) does not enumerate the `all` expansion |
| 11 | `core/src/mev/detectors/mod.rs.temp` | delete (stray artifact) |

### G7 — gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mev-scout-core golden
cargo test -p mev-scout-core --test explorer_golden
```

Then record in §5 (wall-clock vs `main` over the corpus window, budget ≤ 20%) and
§6.3 (first scanner backrun precision/recall against
`explorer::golden::score_embedded_causal_set`). Both need an RPC-backed corpus run
(`MEV_SCOUT_E2E=1` + `RPC_URL`); if unavailable, record `not run — no RPC baseline`
rather than leaving the line blank.

**Result (2026-10-07).** fmt clean; clippy `-D warnings` clean; `cargo test --workspace`
green (25 test binaries, 0 failures); `golden` + `explorer_golden` green. §5 and §6.3
recorded as `not run — no RPC baseline` above.

### Product bugs found while closing G5 (pre-existing, fixed)

The §6.1 tests exposed three defects outside the gap list; all fixed:

1. **multi_hop never emitted 2-pool cycles** — `check_path` derived the entry token as
   "non-shared side of the first pool", which is ambiguous when both pools hold both
   tokens, and the one-directional spot prefilter rejected the cycle whenever the
   profitable rotation opposed the lexicographic path order. Fixed: for 2-pool paths the
   prefilter now accepts either orientation, and same-pair cycles price both rotations
   at the canonical entry token (max address — matching the token pair two_hop derives
   from its shared-token bucket).
2. **`arbitrage_pairs` was process-nondeterministic** — same-pair cycles took their
   `shared_token` from whichever `token_index` bucket the HashMap visited first, so
   two_hop's reported token pair varied per run. Fixed: buckets iterate in ascending
   address order.
3. **Same-token profits never cleared gas** — `normalize_profit` returned
   `token_in == token_out` profits as raw token units while `gas_cost_wei` is native;
   a cycle entered with e.g. USDC reported ~1e11 against ~1e16 gas, so D3's `net(post)`
   could never fire. Fixed: equal-token profits convert through `normalize_to_native`
   (identity for the native token, as-is fallback when unpriceable).

### Remediation checklist

- [x] G1 — pre-image stored from `S_{i-1}`, fresh-per-tx detector instances, no re-detect in `post_detect`
- [x] G2 — stamping loops skip `Strategy::Backrun`; anchor rule per §2.5
- [x] G3 — `suppress_superseded_arbs` + both call sites (before `retain_with_rejections`)
- [x] G4 — `sync_block_from_logs` pre/post/suppression wiring
- [x] G5 — `core/tests/backrun.rs` (10 tests)
- [x] G6 — docs/config/stale-comment table (11 items)
- [x] G7 — fmt + clippy + workspace tests + golden; record §5 and §6.3 numbers above
