# Aggregator-assisted atomic arbitrage: measure first, then add a strict opt-in mode

Status: proposed  
Date: 2026-11-24  
Applies to: `explorer index` / atomic `arb_atomic` classification

## 0. TL;DR

An atomic arbitrage whose cycle **requires an aggregator/router leg** (for example
`V3 → DEX → 1inch`) is currently **not detected**. This is intentional: router legs are
excluded from cycle enumeration to avoid the KyberSwap false positives documented in
`core/src/explorer/scenarios.rs:85-102`.

Recommended implementation order:

1. **Phase 0 — diagnostic only.** Add a dedicated reject reason and emit it when a tx has
   router provenance but the DEX-only graph cannot close. Seed the corpus with real hashes
   found this way. No classification or P&L behavior changes.
2. **Phase 1 — strict opt-in mode.** Add `--aggregator-cycles-strict`, default off. In this
   mode, admit a router edge into the cycle graph only if it passes strict provenance,
   single-path, token-source, and amount-continuity guards. Emit at most one event per tx,
   with `cycle_basis="aggregator_assisted"` and `underlying_pools_unknown=true`.
3. **Phase 2 — optional, later.** Targeted `debug_traceTransaction` only for candidates
   rejected in Phase 0, to reconstruct inner router pools. Do **not** enable global tracing.

Do **not** simply remove the aggregator exclusion. It reintroduces known false positives.

---

## 1. Evidence for the current behavior

| Fact | Location |
|---|---|
| Router legs are decoded as `Amm::Aggregator` | `core/src/explorer/decode/aggregator.rs:10-17,35-108` |
| Only five router topic families are recognized | `core/src/explorer/decode/mod.rs:124-128` |
| `Amm::Aggregator` is a real `Amm` variant | `core/src/explorer/types.rs:336-338` |
| Cycle-edge filter explicitly rejects `Amm::Aggregator` | `core/src/explorer/classify/legs.rs:87-109` |
| Route enumeration uses only filtered edges | `core/src/explorer/classify/legs.rs:162-204` |
| Arb pass repeats the exclusion and cites the KyberSwap FP | `core/src/explorer/classify/arb.rs:156-165` |
| Aggregator-only cycle produces no arb | `core/src/explorer/classify/legs.rs:396-414`, `core/src/explorer/classify/arb.rs:1361-1390` |
| Router path is opaque in logs-only mode | `core/src/explorer/classify/arb.rs:459-465` |
| No trace RPC is used | `core/src/explorer/ingest.rs:1116-1122` |
| Plan already labels this case `Unsupported — FN for genuine mixed cycles` | `docs/arb_atomic_audit_gaps_plan.md:308,356,384` |

Current verdict: **partial**. Direct V3/V2/V4 DEX cycles are detected. Router identity is
retained as metadata (`router_version`, `underlying_pools_unknown`), but a router edge cannot
close a cycle.

---

## 2. Hard guardrails

These must remain true in both phases.

1. **No new `MevKind`.** Still emit `arb_atomic`. See `docs/arb_atomic_audit_gaps_plan.md:3-7,475`.
2. **No change to `compute_net_profit` / basis `R`.** P&L authority stays the existing ledger
   path in `core/src/explorer/profit.rs`.
3. **At most one `MevEvent` per tx.** `select_route` already does one-event-per-tx
   (`core/src/explorer/classify/arb.rs:167-195`); keep it.
4. **Default behavior is bit-identical.** With the new flag off, live `explorer index` output,
   canonical IDs, dashboard counts, and benchmark numbers must not change.
5. **No `factory` weakening and no skim changes.** This plan is unrelated to the empty
   `v2_like` registry.
6. **Do not add a generic `routing` tag.** The vocabulary is fixed in
   `core/data/explorer_tags.json:1-28`; use `details` fields instead.
7. **Do not persist global traces.** Phase 2, if ever implemented, is per-candidate and cached.

---

## 3. Phase 0 — diagnostic reject + corpus seeds

Goal: make the FN measurable without changing what is classified.

### 3.1 Add a reject reason

File: `core/src/explorer/reject.rs`

Add a variant next to `UnresolvedCycle` (around line 49):

```rust
/// Explorer classify: a router/aggregator was present and the DEX-only graph
/// could not close. Diagnostic for genuine mixed-cycle FNs; no event is emitted.
AggregatorCycleSkipped,
```

Add the mapping in `RejectReason::as_str` (around line 112):

```rust
RejectReason::AggregatorCycleSkipped => "aggregator_cycle_skipped",
```

Do not rewrite existing `unresolved_cycle` rows.

### 3.2 Emit the diagnostic in the no-route branch

File: `core/src/explorer/classify/arb.rs`, lines 179-194.

Current logic pushes `RejectReason::UnresolvedCycle` when `resolved_edges.len() >= 2`.
Change only that branch:

```rust
let has_router = tx.route_provenance.is_some()
    || tx.swaps.iter().any(|s| s.amm == Amm::Aggregator);

if resolved_edges.len() >= 2 || has_router {
    let reason = if has_router {
        RejectReason::AggregatorCycleSkipped
    } else {
        RejectReason::UnresolvedCycle
    };
    let detail = serde_json::json!({
        "router_version": tx
            .route_provenance
            .as_ref()
            .and_then(|p| p.router_version.as_deref()),
        "dex_edges": resolved_edges.len(),
        "aggregator_swaps": tx
            .swaps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.amm == Amm::Aggregator)
            .map(|(i, s)| serde_json::json!({
                "swap_index": i,
                "log_index": s.log_index,
                "token_source": s.token_source.as_str(),
                "amount_in": s.amount_in.to_string(),
                "amount_out": s.amount_out.to_string(),
            }))
            .collect::<Vec<_>>(),
        "mode": "diagnostic",
    })
    .to_string();
    rejects.push(classify_reject(
        input.block,
        tx.tx_index,
        reason,
        Some(detail),
    ));
}
```

Notes:

- `classify_reject` takes `detail: Option<String>` (`arb.rs:741-764`), so serialize JSON to a
  string exactly as above.
- Keep the existing `continue` after this branch.
- Rejects are persisted only when `record_rejections` is enabled
  (`core/src/explorer/ingest.rs:91-93,171`). For measurement, enable it in a temporary config:
  `backtest.record_rejections = true`. Do not commit a config containing RPC keys.

### 3.3 Tests for Phase 0

File: `core/src/explorer/classify/arb.rs` test module.

Add:

1. `diagnostic_reject_for_open_mixed_cycle`
   - Build a tx with two non-aggregator edges that do not close plus one `Amm::Aggregator` edge.
   - Set `tx.route_provenance = Some(RouteProvenance { router_version: Some("oneinch_v4_v5".into()) })`.
   - Call `classify_atomic_arbs(&input, &tag_opts())` with strict mode still off.
   - Assert: **no** `arb_atomic` event; one reject with reason `aggregator_cycle_skipped`;
     detail JSON has `router_version == "oneinch_v4_v5"`.

2. `plain_open_cycle_still_uses_unresolved_cycle`
   - Same but no router/provenance.
   - Assert reason remains `unresolved_cycle`.

Keep these existing tests unchanged and passing:

- `resolved_edges_exclude_aggregator_legs` (`legs.rs:375-394`)
- `aggregator_only_cycle_is_not_closed` (`legs.rs:396-414`)
- `aggregator_only_cycle_produces_no_arb` (`arb.rs:1361-1390`)
- `aggregator_leg_does_not_close_arb_cycle` (`scenarios.rs:85-102`)

### 3.4 Corpus seeds

File: `core/tests/explorer_corpus.rs`.

Do **not invent a hash**. Mine candidates first:

1. Enable `record_rejections = true` in a temporary config.
2. Run `explorer index` on a few recent windows.
3. Query `rejected_candidates` for `reject_reason = 'aggregator_cycle_skipped'`.
4. Manually verify at least one tx on-chain/with logs before seeding.

Add a TODO entry with a verified hash:

```rust
// TODO(replace-with-verified-hash): aggregator-assisted open cycle.
// Default mode: no arb event; reject reason aggregator_cycle_skipped.
```

If no real candidate is found after mining, record that in the plan and stop: the FN rate is
likely too low to justify Phase 1.

### 3.5 Phase 0 acceptance

- `cargo test -p mev-scout-core explorer::classify` passes.
- `cargo test -p mev-scout-core --test explorer_corpus` passes.
- Flag-off benchmark dashboard still reports `ops=111` for
  `99833048..=99836394`.
- At least one real reject hash is verified and seeded, or the decision is explicitly
  documented to stop.

---

## 4. Phase 1 — strict opt-in mixed-cycle mode

Only start this after Phase 0 finds real mixed-cycle FNs.

### 4.1 Add the CLI flag

File: `cli/src/cli.rs`, inside `IndexArgs` after `discover_pools` (line 149-150):

```rust
/// Allow a strictly validated aggregator/router leg to close an atomic cycle.
/// Default off: benchmark and historical classifications stay unchanged.
#[arg(long, help_heading = "Index")]
pub aggregator_cycles_strict: bool,
```

A boolean is intentional and simpler than an enum. It corresponds to the previously discussed
`--aggregator-cycles strict`. If a third mode is ever needed, convert it to a `ValueEnum`.

### 4.2 Plumb the flag through

Do these edits in order:

1. `cli/src/commands/mod.rs:63-71`: add `a.aggregator_cycles_strict` to the `cmd_backfill(...)`
   call.
2. `cli/src/commands/explorer/backfill.rs:19-28`: add parameter
   `aggregator_cycles_strict: bool`.
3. `cli/src/commands/explorer/backfill.rs:32-33`: after `let mut cfg = ...` equivalent, set:

   ```rust
   cfg.aggregator_cycles_strict = aggregator_cycles_strict;
   ```

   `cmd_backfill` already has `#[allow(clippy::too_many_arguments)]` at line 19, so no clippy
   change is needed.

4. `core/src/explorer/ingest.rs:66-103`: add to `IngestConfig`:

   ```rust
   /// Admit strictly validated router edges into atomic cycle enumeration.
   /// Default false. Set by CLI `--aggregator-cycles-strict`, never by config file.
   pub aggregator_cycles_strict: bool,
   ```

5. `core/src/explorer/ingest.rs:138-158`: initialize `aggregator_cycles_strict: false` in
   `from_chain`.
6. `core/src/explorer/ingest.rs:255-275`: in `run_block_context`, add:

   ```rust
   aggregator_cycles_strict: cfg.aggregator_cycles_strict,
   ```

7. `core/src/explorer/classify/mod.rs:124-155`: add to `BlockContext`:

   ```rust
   /// Strict aggregator-assisted cycle mode. Default false.
   pub aggregator_cycles_strict: bool,
   ```

8. `core/src/explorer/classify/mod.rs:157-175` and `BlockContext::for_tests` (around line 178):
   initialize to `false`.

9. `core/src/explorer/classify/arb.rs:41-44`: `classify_atomic_arbs` already has `input.ctx`;
   no new parameter needed.

No TOML/config schema change is required.

### 4.3 Strict edge admission

File: `core/src/explorer/classify/legs.rs`.

Keep `is_cycle_edge` unchanged (line 104-109) for default mode. Add helpers:

```rust
fn is_strict_aggregator_edge(tx: &TxInput, swap_index: usize) -> bool {
    let s = &tx.swaps[swap_index];
    if s.amm != Amm::Aggregator || has_unresolved_tokens(s) {
        return false;
    }
    if s.token_in == decode::TOKEN0_SENTINEL
        || s.token_in == decode::TOKEN1_SENTINEL
    {
        return false;
    }
    // Proximity-guessed direction is never enough to close a cycle.
    if s.token_source == LegSource::Proximity {
        return false;
    }
    if s.amount_in == U256::ZERO || s.amount_out == U256::ZERO {
        return false;
    }
    // Only known router topic families carry a version label.
    let known_router = tx
        .route_provenance
        .as_ref()
        .and_then(|p| p.router_version.as_deref())
        .is_some();
    known_router && router_is_single_path(tx, s)
}

fn router_is_single_path(tx: &TxInput, agg: &SwapFact) -> bool {
    let ins = tx
        .transfers
        .iter()
        .filter(|t| t.token == agg.token_in && t.to == agg.pool && t.amount != U256::ZERO)
        .count();
    let outs: Vec<_> = tx
        .transfers
        .iter()
        .filter(|t| t.token == agg.token_out && t.from == agg.pool && t.amount != U256::ZERO)
        .collect();
    ins == 1 && outs.len() == 1 && outs[0].to != agg.pool
}
```

`TransferFact` fields are at `core/src/explorer/types.rs:294-304`.

Change `resolve_arb_routes(tx)` to `resolve_arb_routes(tx, strict: bool)`:

- Filter indices with:

  ```rust
  .filter(|(i, s)| is_cycle_edge(s) || (strict && is_strict_aggregator_edge(tx, *i)))
  ```

- After `closed_cycle_walks(&edges)`, when `strict == true`, discard any walk with no
  non-aggregator edge. A strict mixed cycle must contain **at least one real DEX leg** and
  **at least one router leg**; aggregator-only cycles stay rejected.

Change `resolved_cycle_edges(tx)` to `resolved_cycle_edges(tx, strict: bool)` using the same
filter so the reject branch sees the same edge set.

Update all call sites:

- `core/src/explorer/classify/arb.rs:164-165`
- tests in `legs.rs` / `arb.rs`

### 4.4 Tight amount continuity

Add in `legs.rs` (or `profit.rs` if already similar logic exists):

```rust
fn amount_chain_ok(a: U256, b: U256) -> bool {
    if a == b {
        return true;
    }
    if a == U256::ZERO || b == U256::ZERO {
        return false;
    }
    let diff = if a > b { a - b } else { b - a };
    let min = if a < b { a } else { b };
    // <= 1 bp; much tighter than the 200/400 bps aggregator fact-dedup tolerance.
    diff * U256::from(10_000u64) <= min
}
```

In `resolve_arb_routes`, when `strict == true`, discard a candidate walk unless **every**
consecutive edge satisfies `amount_chain_ok(prev.amount_out, next.amount_in)`, including the
last-to-first wrap. Use `ResolvedCycleEdge.amount_in/amount_out` already present
(`legs.rs:115-125`).

Do **not** use the 200/400 bps tolerances from
`core/src/explorer/decode/aggregator.rs:130-161,180-235` here; those only dedup redundant
facts.

### 4.5 Ownership with a neutral router

File: `core/src/explorer/classify/arb.rs:1039-1066`.

Change `flow_attributable` to take a third argument:

```rust
pub(crate) fn flow_attributable(
    swaps: &[SwapFact],
    candidates: &[Address],
    neutralize_aggregators: bool,
) -> bool
```

Inside, build `route_pools` as today, then if `neutralize_aggregators` is true also insert
pools of `s.amm == Amm::Aggregator` that are direction-resolved. This makes a router neutral
**only when it is part of the selected route**, not globally trusted.

Call at `arb.rs:176`:

```rust
flow_attributable(&swaps, &candidates, input.ctx.aggregator_cycles_strict)
```

Update every test call site to pass `false` unless the test is specifically about strict mode.

### 4.6 Event metadata and canonical ID

File: `core/src/explorer/classify/arb.rs`, around lines 454-509.

After the selected `route` is known:

```rust
let has_router_edge = route.edges.iter().any(|e| e.amm == Amm::Aggregator);
if has_router_edge {
    details["cycle_basis"] = serde_json::json!("aggregator_assisted");
}
```

Keep the existing metadata:

- `underlying_pools_unknown = true` from `tx.route_provenance` (`arb.rs:463-465`)
- `router_version` (`arb.rs:503-509`)
- `underlying_pools_hash` (`arb.rs:454-458`)

In strict mode `route.pools()` now includes the router address, so
`underlying_pools_hash` distinguishes router identity. Keep the `underlying_pools_unknown`
flag: internal router pools are still opaque.

At `arb.rs:497-502`, force `"inferred"` confidence for router-assisted routes:

```rust
let exact = routes.len() == 1
    && unattributed_flow == 0
    && !has_router_edge;
details["classification_confidence"] =
    serde_json::json!(if exact { "exact" } else { "inferred" });
```

Do not add a new canonical suffix format. Existing
`canonical_route_suffix(hash, router_version, funding_source)` at
`core/src/explorer/canonical.rs:122-136` already incorporates the hash and router version.

### 4.7 Phase 1 tests

`core/src/explorer/classify/legs.rs`:

- `strict_mode_includes_single_path_aggregator_edge`
- `strict_mode_rejects_proximity_aggregator_edge`
- `strict_mode_rejects_split_router_output`
- `strict_mode_rejects_amount_gap_above_one_bp`
- `strict_mode_requires_at_least_one_dex_edge`

`core/src/explorer/classify/arb.rs`:

- `mixed_aggregator_v3_cycle_emits_one_event_when_strict`
  - Assert one `arb_atomic`, `details.cycle_basis == "aggregator_assisted"`,
    `details.router_version == <known label>`, `details.classification_confidence == "inferred"`.
- `same_mixed_cycle_is_not_emitted_when_flag_off`
  - Same input, `ctx.aggregator_cycles_strict = false`.
  - Assert zero events and reject reason `aggregator_cycle_skipped`.
- `flow_attributable_neutralizes_only_route_router`
  - A router pool in the route is neutral; an unrelated router owner still fails.
- Existing `aggregator_only_cycle_produces_no_arb` must pass in **both** modes.

`core/tests/explorer_corpus.rs`:

- For each seeded real hash, assert:
  - default mode: no `arb_atomic`, reject `aggregator_cycle_skipped`;
  - strict mode: either one `arb_atomic` with `cycle_basis="aggregator_assisted"`, or a reject
    whose detail names the exact guard that failed. Never a silent skip.

### 4.8 Phase 1 acceptance

- `cargo check -p mev-scout-cli` passes.
- `cargo test -p mev-scout-core explorer::classify` passes.
- `cargo test -p mev-scout-core --test explorer_corpus` passes.
- Flag-off idempotent index on `99833048..=99836394` still reports `ops=111` and does not
  change stored `canonical_id` values.
- A strict-mode run on a seeded candidate window emits exactly one event per candidate tx;
  no `MevKind` other than `arb_atomic` is added.
- Dashboard KPI remains `ops.len()` after the forensic-action filter
  (`core/src/explorer/dashboard/mod.rs:774-778`). No dashboard code change expected.

---

## 5. Optional Phase 2 — targeted trace reconstruction

Do not implement in the MVP. Consider only if Phase 1 still misses verified candidates.

1. Select txs with `rejected_candidates.detail` reason `aggregator_cycle_skipped`.
2. Fetch `debug_traceTransaction` with `callTracer` **only for those tx hashes**.
3. Reconstruct pool-to-pool ERC-20 transfers inside the router and synthesize `SwapFact` rows
   with `token_source = LegSource::Transfer`.
4. Cache the derived facts in `tx_evidence` keyed by `(chain, tx_hash)`; never refetch on every
   backfill.
5. Re-run strict classification with the synthesized inner edges.
6. No global trace RPC, no change to `tx_evidence` schema without a migration test.

This phase needs its own plan and benchmark before approval.

---

## 6. File-by-file checklist

Implement in this order; stop and run tests after each numbered block.

1. `core/src/explorer/reject.rs` — add `AggregatorCycleSkipped` + `as_str`.
2. `core/src/explorer/classify/arb.rs` — Phase 0 reject branch + detail JSON.
3. `core/src/explorer/classify/arb.rs` tests — diagnostic reject tests.
4. `core/tests/explorer_corpus.rs` — verified seed placeholder(s) from reject mining.
5. `cli/src/cli.rs` — `--aggregator-cycles-strict`.
6. `cli/src/commands/mod.rs` — pass the flag.
7. `cli/src/commands/explorer/backfill.rs` — accept flag, set `cfg.aggregator_cycles_strict`.
8. `core/src/explorer/ingest.rs` — `IngestConfig` field + `run_block_context` copy.
9. `core/src/explorer/classify/mod.rs` — `BlockContext` field + defaults.
10. `core/src/explorer/classify/legs.rs` — strict admission, continuity, route filter.
11. `core/src/explorer/classify/arb.rs` — `flow_attributable` param, details, confidence.
12. Tests in `legs.rs`, `arb.rs`, `explorer_corpus.rs`.
13. Verification commands from section 7.

---

## 7. Verification commands

Baseline before code changes (flag off):

```powershell
@'
import sqlite3, json
p = r"cache\explorer-avalanche.sqlite"
c = sqlite3.connect("file:" + p + "?mode=ro", uri=True)
rows = c.execute(
    "SELECT canonical_id FROM mev_ops "
    "WHERE kind='arb_atomic' AND block_number BETWEEN 99833048 AND 99836394 "
    "ORDER BY canonical_id"
).fetchall()
open("arb_ids_before.json", "w", encoding="utf-8").write(
    json.dumps([r[0] for r in rows], indent=2)
)
'@ | python -
```

After the change, flag-off index should be a no-op and stored IDs unchanged:

```powershell
.\target\debug\mev-scout.exe --config mev-scout.toml explorer index `
  --from-block 99833048 --to-block 99836394
```

Expected: `Backfill done — 0 new blocks indexed, 0 ops`, dashboard `ops=111`.

Re-run the baseline query into `arb_ids_after.json` and diff.

Strict-mode smoke test on a **small** window containing a mined candidate:

```powershell
.\target\debug\mev-scout.exe --config mev-scout.toml explorer index `
  --from-block <FROM> --to-block <TO> `
  --aggregator-cycles-strict --reclassify
```

Do not use the 3347-block derivation window for this smoke test; a reclassify there costs about
15 minutes and changes benchmark conditions.

Mining rejects (temporary config with `backtest.record_rejections = true`):

```powershell
python -c "import sqlite3; c=sqlite3.connect(r'file:cache/explorer-avalanche.sqlite?mode=ro',uri=True); print(c.execute(\"select block_number,tx_index,detail from rejected_candidates where reject_reason='aggregator_cycle_skipped' order by block_number limit 20\").fetchall())"
```

---

## 8. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Removing router exclusion brings back KyberSwap FPs | Default off; router edge only admitted with known topic, trusted token source, single path, tight amount chain, and route-level ownership. |
| Router is neutralized globally | `flow_attributable` only neutralizes router pools present in the selected route, and only in strict mode. |
| Canonical ID collisions | `route.pools()` includes the router address in strict mode; existing hash/router suffix remains. |
| Split routes falsely accepted | Require exactly one inbound and one outbound transfer for the router token pair; otherwise reject with detail. |
| Unknown router signature silently ignored | `router_version` must be present; unmapped logs remain unmapped and produce no mixed event. |
| Dashboard count changes | No dashboard code change; forensic filter still applies; `ops.len()` unchanged. |
| Benchmark comparability lost | Flag off is bit-identical; never run `--reclassify` on the frozen benchmark as part of acceptance. |
| Cost explosion from tracing | Phase 2 is per-candidate and out of MVP scope. |

---

## 9. Definition of done

- [ ] Phase 0 reject reason exists and appears for at least one verified real tx.
- [ ] Default-mode benchmark numbers and canonical IDs are unchanged.
- [ ] Strict flag is off by default and documented in `--help`.
- [ ] Strict mode emits exactly one `arb_atomic` per qualifying tx with
      `cycle_basis="aggregator_assisted"`.
- [ ] Known FP fixtures still produce zero events in both modes.
- [ ] All listed tests pass.
- [ ] No new `MevKind`, no P&L-basis change, no global trace RPC, no config-key leakage.
