# Dashboard audit fixes + focused visual polish

## Context

The offline dashboard was audited against `.cursor/plans/dashboard_visual_redesign_51b3cd3d.plan.md` and the `vibe_images` concepts. The dark multi-view shell is largely in place, but three verified content defects block acceptance:

1. `reduce_strategy_series` drops valid ops in the first partial bucket (`reduce.rs:303-305`), so pulse/heatmap disagree with KPIs and `series`.
2. Analytics scatter tooltip throws `TypeError` because it reads `p.data` as an array while ECharts items are `{value,itemStyle}` (`dashboard_template.html:1141-1147`).
3. ~~Global filters inconsistently applied~~ → **decision: remove the global filter bar entirely** (Strategy / Confidence / Pricing / Scope / Tags / Reset). Panels always show full-window Rust aggregates. Keep topbar search for tx/address/block lookup only.

Secondary gaps: KPI cards still ship decorative/fake spark boxes (remove them), route edges without humanized amounts, missing `flash_fee_unpriced` surfacing, stale CLI template test, and a less compact/less polished visual pass than the reference.

Goal: apply the recommended fixes end-to-end while preserving the Rust emit-only pipeline, offline single-file HTML, additive JSON/CSV compatibility, and all non-negotiable data semantics.

## Recommended approach

### 1. Rust read-model fixes (`core/src/explorer/dashboard/reduce.rs`)

- Fix first-bucket parity: floor `window.since_ts` to `bucket_secs` before comparing, mirroring `reduce_series` (`reduce.rs:500`). Keep the `until_ts` guard.
- Add `confidence: String` to `StrategySeriesPoint`; key by `(bucket, slice_key, confidence)`.
- Extend `SeriesPoint` with real per-bucket `gross_usd`, `gas_usd`, `priced_ops`, `searchers` (unique competitors). Populate in the existing `reduce_series` loop.
- Extend `ScatterPoint` with `eoa`, `contract`, `block_number` for fallback/search parity.
- Add a compact interned full-window `OpIndex` to `ReducedPanels`:
  - intern tables: `slices`, `actors`, `eoas`, `contracts`, `tags`, `pools`, `amms`, `tx`
  - positional rows: `[ts, slice_id, conf, flags, actor_id, eoa_id, contract_id, block, tx_id, net, gross, gas, vol, flash, tag_ids, legs]`
  - `flags` bitmask: excluded_from_net, volume_partial, usd_approximate, native_unpriced, multi_asset_pricing, pricing_clamped, flash_fee_unpriced
  - `legs` from existing `parse_route`, deduped `(pool_id, amm_id)`
  - built from full `ops`, not truncated `events`
- Extend `TruncationInfo`: `index_total`, `index_emitted`, `index_complete`.
- Add unit tests: first-bucket parity, confidence split, extended series fields, scatter fields, index lossless parity vs reducers.

### 2. Evidence/route fixes (`core/src/explorer/dashboard/evidence.rs`)

- Add `amount_in_human` / `amount_out_human` to `RouteLeg`.
- Populate via existing `humanize_amount(token, amount)` (`evidence.rs:210-217`) inside `parse_route`; unknown decimals stay `None`/raw-only.
- Enrich each leg with token metadata for UI: `token_in_symbol` / `token_out_symbol`, `token_in_name` / `token_out_name` (from `known_tokens.json` when present; otherwise leave `None` and let HTML fall back to short address).
- Keep raw `token_in` / `token_out` addresses always present for display + copy.
- Template maps `amm` → DEX/AMM icon (shared with palette); unknown AMM → generic pool glyph.
- Add tests for known-token humanization, symbol/name enrichment, and unknown-token fallback.

### 3. Budget/truncation (`core/src/explorer/dashboard/mod.rs`)

Rewrite `enforce_budget` (`mod.rs:896-916`) with deterministic trim order:

1. clear `evidence`
2. halve `events` to floor 50
3. halve `scatter_points` to floor 200
4. clear `op_index` if still over budget; set `index_emitted=0`, `index_complete=false`

`hit_budget=true` whenever any stage fires. Budget remains soft; never silently exceed without a banner.

HTML policy:

- No global filter bar. KPIs/mix/heatmap/searchers/pools/Analytics always = full-window emit.
- `op_index` (when complete) powers topbar search + ops list / drill-down only — not panel re-aggregation.
- index absent/incomplete: search may degrade; show explicit budget/truncation banner. Events/scatter may be emitted subsets with truncation wording. Never present subset KPIs as full-window totals.

### 4. Template JS rewiring (`core/data/dashboard_template.html`)

- Fix Analytics scatter tooltip: `var d = p.data.value || p.data;` (match Pools `:972`). Include tx short + `VOLUME_PARTIAL`.
- **Remove** `#filter-bar` markup, CSS, chip selects, tag chips, reset, filter-meta, and all filter event wiring / `filteredRows` panel rewiring.
- Keep topbar `#global-search` for tx/eoa/contract/block; optionally use `op_index` to drive the ops list highlight/open when complete.
- Overview/Searchers/Pools/Analytics bind directly to Rust panel payloads (`P.kpis`, `P.series`, …) — no client-side filter re-aggregation.
- **KPI cards: strip spark boxes** — delete `.kpi-spark` containers, `sparkOption`, and ECharts init on KPI cards. Cards = label + value + subtext only (see screenshot intent).
- Page titles: rename `Network overview` → **`Overview`** everywhere (`#page-title` default + `titles.overview` map).
- Drill-down:
  - render humanized route amounts under token pairs; observed AMM labels only
  - for each route token / pool hop: show **symbol + name + token address** (short + copyable full) and **DEX/AMM icon** (not label-only)
  - add `flash_fee_unpriced` badge/facts/waterfall note
  - surface pricing flags (`usd_approximate`, `excluded_from_net`, native/multi/clamped) as badges/facts
  - index-driven ops list search; evidence-cap fallback stub when evidence was not emitted
- Budget fallback shows truncation banner only (no filter-disable path).

### 5. Focused visual polish (no rewrite)

- Compact spacing: card padding/grid gaps/main padding/topbar slightly tighter (no filter bar); chart heights reduced so Analytics scatter is reachable in a normal desktop viewport while preserving the reference widget order.
- Overview mix/gauge: smaller donut + gauge, less blank space, gauge detail centered cleanly.
- **Live operations** title/header: no text truncation/ellipsis/clipping — full label always visible (fix overflow, flex shrink, or width constraints that cut “Live operations”).
- **Chain identity:** topbar `#page-sub` shows chain **icon + label** (e.g. Avalanche snowflake / AVAX mark for `chain_id === 43114`). Inline SVG or data-URI icons for known chains; generic chain glyph fallback. Offline-only — no CDN.
- Explicit AMM palette (`v2`, `v3`, `v4`, `lb`, etc.) shared by treemaps and route nodes so families are separable.
- USD axis label helper with `≈` on pulse secondary axis, net bars, scatter axes, waterfall.
- Route diagram: amount-bearing edges, AMM-colored nodes, horizontal scroll for long routes, non-registry/proximity caption; token nodes show symbol/name/address + DEX icon (see §4).
- Artifact-themed scrollbars for table wraps, ops list, raw JSON.
- Keep dark night-ops palette, sidebar IA, semantics footer, and offline constraints.

### 6. Tests/docs

- Update stale `cli/tests/cli_dashboard.rs:86-95` to assert the current multi-view/offline shell instead of hero/Fraunces/#F7F4EF/no-nav.
- Extend `render.rs` template assertions for index/derive helpers and offline guards.
- Add `core/tests/explorer_dashboard_filters.rs` synthetic fixture: unpriced, VOLUME_PARTIAL, first-partial-bucket, exact/inferred, utility subkinds, multiple actors/pools, excluded_from_net, flash_fee_unpriced, known-token route amounts, budget trim (name kept; asserts emit/index/budget, not UI filters).
- Document additive `op_index` (search/ops only), removed global filters, and budget fallback in `docs/CLI.md`.

## Critical files

- `D:\Repositories\mev-scout\core\src\explorer\dashboard\reduce.rs`
- `D:\Repositories\mev-scout\core\src\explorer\dashboard\evidence.rs`
- `D:\Repositories\mev-scout\core\src\explorer\dashboard\mod.rs`
- `D:\Repositories\mev-scout\core\data\dashboard_template.html`
- `D:\Repositories\mev-scout\core\src\explorer\dashboard\render.rs`
- `D:\Repositories\mev-scout\cli\tests\cli_dashboard.rs`
- `D:\Repositories\mev-scout\core\tests\explorer_dashboard_filters.rs` (new)
- `D:\Repositories\mev-scout\docs\CLI.md`

## Reuse

- `reduce_series` bucket flooring (`reduce.rs:500`)
- `parse_route` / `RouteLeg` (`evidence.rs:55-76`)
- `humanize_amount` + known-token decimals (`evidence.rs:177-217`)
- `escape_json_for_script` (`evidence.rs:223-225`)
- Pools scatter tooltip guard (`dashboard_template.html:972`)
- Existing `chain_id` / Snowtrace branch (`dashboard_template.html:1330-1338`) extended for chain icon map
- `ExplorerStore` fixture seeding pattern from `mod.rs` tests and `cli/tests/cli_dashboard.rs`

## Verification

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test -p mev-scout-core explorer::dashboard`
4. `cargo test -p mev-scout-cli --test cli_dashboard`
5. `cargo test --workspace`
6. Regenerate real artifacts:
   - `target/debug/mev-scout.exe explorer dashboard --db cache/explorer-avalanche.sqlite --window all --out results/dash.html --with-gaps`
   - same with `--format json`
7. Browser acceptance (Chrome/browser-use):
   - no console errors; no external requests
   - page title is **Overview** (not “Network overview”); chain icon visible next to chain label for Avalanche (and known chains)
   - Overview KPIs reconcile with JSON; pulse bars include first-bucket ops; **no KPI spark/chart boxes**
   - filter bar absent; panels always full-window; topbar search still finds/opens ops
   - Analytics + Pools scatter tooltips render slice/volume/net/tx/VOLUME_PARTIAL
   - fixture drill-down: humanized route amounts, token symbol+name+address + DEX icon, flash-fee-unpriced note, pricing badges, collapsed raw JSON, deep link
   - “Live operations” fully visible (no truncated title) on Overview desktop + mobile
   - budget fixture: explicit banner; no silent partial aggregates
   - desktop 1440×1000 and mobile 390×844: no overflow/clipping, charts resize, view toggles stable
8. Confirm DB bytes unchanged and CSV header unchanged.

## Execution order

1. Rust reducers/evidence + unit tests
2. Budget/truncation + integration fixtures
3. Template: remove filter bar; bind panels to Rust payloads; tooltip fix; search/index for ops only
4. Drill-down route/flash/pricing + token/DEX metadata
5. Visual polish: Overview title, chain icons, strip KPI sparks, Live operations, spacing/AMM palette
6. CLI/render tests + docs
7. Regenerate + browser acceptance + repair/rerun
8. Final fmt/clippy/test gates