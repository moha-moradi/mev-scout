---
name: Dashboard visual redesign
overview: Redesign the offline forensic dashboard into a multi-view, ECharts-powered UI (Overview → Analytics → Operation drill-down → Data Health), binding only to DashboardSnapshot semantics—never inventing USD zeros, confidence %, branded DEX names, or headline volume.
todos:
  - id: phase0-scaffold
    content: "Phase 0: dark multi-view shell, vendored ECharts offline, shared USD/null/subkind helpers, update render.rs tests"
    status: completed
  - id: phase1-overview
    content: "Phase 1: Overview KPIs, profit/activity, subkind donut, priced gauge, searchers, live ops + unpriced badge"
    status: completed
  - id: phase2-analytics-reduce
    content: "Phase 2: add strategy_series (and capped scatter points) in reduce.rs; Analytics heatmap/treemap/log scatter"
    status: completed
  - id: phase3-op-drill
    content: "Phase 3: Operations list + drill-down waterfall, V3 route diagram, evidence tags, collapsed raw JSON"
    status: completed
  - id: phase4-data-health
    content: "Phase 4: Data Health view from coverage/gaps/quality/caps/rejects/storage; retire details-drawer as primary"
    status: completed
isProject: false
---

# MEV Scout Dashboard Visual Redesign

## Goal

Replace the single-scroll cream “forensic essay” UI in [`core/data/dashboard_template.html`](core/data/dashboard_template.html) with a multi-view analytics shell that matches the **data-driven** vibe concepts under [`vibe_images/`](vibe_images/) (dark night-ops + real sample magnitudes), while keeping the existing Rust pipeline:

`explorer dashboard` CLI → [`build()`](core/src/explorer/dashboard/mod.rs) → `DashboardSnapshot` → [`render::to_html`](core/src/explorer/dashboard/render.rs) → self-contained HTML.

[`cli/src/commands/explorer/dashboard.rs`](cli/src/commands/explorer/dashboard.rs) stays an emit/write command. Visual work lives in the template + small read-model additions in [`reduce.rs`](core/src/explorer/dashboard/reduce.rs) / [`evidence.rs`](core/src/explorer/dashboard/evidence.rs).

```mermaid
flowchart LR
  sqlite[explorer SQLite RO] --> build[dashboard::build]
  build --> snap[DashboardSnapshot JSON]
  snap --> html[dashboard_template.html]
  html --> views[Overview Analytics Ops DataHealth]
  views --> echarts[Vendored ECharts]
```

## Vibe images = visual acceptance target

Yes: after the phases land, the shipped HTML should **feel and read like** [`vibe_images/`](vibe_images/), not like the current cream single-scroll template. That is the definition of done for UI look/IA.

**Primary targets (data-driven concepts — match layout + widgets):**

| Image | Ships in | Must match |
|-------|----------|------------|
| `mev-scout-overview-data-concept_*.png` | Phase 1 | Sidebar, 6 KPIs (no headline volume), profit+activity, subkind donut, priced gauge, top searchers, live ops + unpriced badge |
| `mev-scout-analytics-data-concept_*.png` | Phase 2 | Heatmap by subkind×time, mix + exact/inferred, net-by-strategy, AMM treemap, log profit-vs-volume scatter + `VOLUME_PARTIAL` tooltip + unpriced caption |
| `mev-scout-operation-data-concept_*.png` | Phase 3 | Op header badges, 4 metrics, profit waterfall, facts, 2-hop AMM route (observed labels), evidence tags, collapsed raw JSON |

**Secondary references (steal widgets / density, not pixel skin):**

- `mev-scout-overview-concept_*` / `mev-scout-analytics-concept_*` / forensic / night-ops assets under chat `assets/` or older vibe files → indexing health gauge ideas, waterfall/route/confidence checklist cues, pulse chart density.

**What “reaching vibe_images” does *not* mean:**

- Pixel-perfect Figma clone or identical axis tick labels from AI mockups
- Hardcoded mock KPIs ($0.93 / 90 ops / 6 JIT) when the open DB differs — numbers always come from `DashboardSnapshot`
- Inventing branded DEX icons (Trader Joe / Pangolin) when the DB only has `amm: "v3"` + pool addresses
- Light cream Fraunces “observatory” skin as the final theme — dark data-concept is the locked skin

**Acceptance check:** open regenerated HTML next to the three `*-data-concept` PNGs; a reviewer should recognize the same views/widgets without needing the old Details drawer essay.

## Non-negotiable data rules (implementer checklist)

Copy these into code comments near the USD formatters / chart builders. Fail review if any are violated.

1. **Null USD ≠ zero.** `usd(null)` renders an em-dash / “unpriced”, never `$0`. Do not `|| 0` on profit/net/gas/volume for display axes.
2. **Unpriced ops stay countable.** Include them in op counts, strategy mix, heatmaps, searcher op counts. Exclude them from **USD axes** (profit line, net bars, scatter USD coords) and show an explicit caption (e.g. “N JIT unpriced — in counts, not USD axes”). Current sample DB: 12/12 JIT unpriced; vibe mock showed 6 — **always bind to `kpi.quality.ops_unpriced` / filtered events**, never hardcode.
3. **No headline aggregate volume.** Overview KPIs = Net, Gross, Ops, Searchers, Gas, Pricing coverage (as in `mev-scout-overview-data-concept`). Volume only in Analytics as **log-scaled** scatter/chart with outlier tooltips (`VOLUME_PARTIAL`, raw `volume_usd`). Live DB max volume ≈ `$1.2e11` with tiny net — that is the motivating case.
4. **Show subkinds.** Never roll `utility` into one bucket. Donuts/heatmaps/filters must split `utility/bundler` vs `utility/claim_and_sell` (and any other `kind/subkind`). Display label helper: `kind === "utility" && subkind ? subkind : kind` with stable colors per slice key `kind + "/" + (subkind||"")`.
5. **Confidence is exact|inferred**, not a fake %. Use badges + evidence checklist; do not invent “98%”.
6. **AMM labels = observed `route[].amm`** (`v2`/`v3`/`v4`/`lb`/aggregator-style strings from DB). Branded DEX names / token icons only when a **reliable** mapping exists (labels table or known registry + `token_source === "registry"`). Otherwise truncate pool address + AMM family.
7. **Semantics footer everywhere:** reuse `snapshot.semantics` (`null_usd_means`, `usd_columns_are`, …). Prefix priced USD with ≈.

## What already exists (reuse, don’t recompute blindly)

| Need | Source |
|------|--------|
| KPIs + priced/unpriced/exact/inferred | `panels.kpi`, `panels.kpi.quality` |
| Time series ops + net + blocks | `panels.series`, `panels.bucket_secs` |
| Kind × subkind × confidence | `panels.kind_matrix` |
| Searchers / pools / tokens | `panels.actors`, `pools`, `tokens` |
| Event table rows | `panels.events` (may be truncated) |
| Drill evidence (route, tags, reasons, details) | `panels.evidence` → `OpEvidence` |
| Coverage / gaps / rejects / caps / storage | `coverage`, `gaps`, `rejects`, `caps`, `storage` |
| Route legs | `evidence::RouteLeg` (`pool`, `amm`, `token_*`, amounts) |

**Do not** derive strategy×time or actor×time charts from truncated `events` for “complete” Analytics. When Analytics needs those, **add aggregates in `reduce.rs` first** (see Phase 2).

## Design decisions locked for handoff

- **Theme:** Dark night-ops / data-concept (deep navy/charcoal, mint/teal gain, coral loss, purple secondary). Avalanche red only as sparse brand accent, not purple-on-white or cream Fraunces paper.
- **IA:** Left sidebar + main stage. Views: **Overview**, **Analytics**, **Operations**, **Data Health**. Operation drill-down is a full panel/route under Operations (not only a thin aside), inspired by `mev-scout-operation-data-concept`.
- **Charts:** Vendored **Apache ECharts** (minify into repo, e.g. `core/data/echarts.min.js`, inlined or `<script>` sibling embedded at generate time). **No CDN** (drop Google Fonts CDN too; use a local/system stack such as IBM Plex or similar already in project spirit).
- **Offline:** One HTML file remains the deliverable; `render.rs` may concatenate template + echarts blob, or template includes a second `/*__ECHARTS__*/` placeholder replaced at render time.
- **Mock numbers:** Screenshots are illustrative. UI must read `DashboardSnapshot` only. Example drill op from concept: net≈`$0.6062`, gross≈`$0.6146`, gas≈`$0.0084` — use whichever arb ranks top in the emitted evidence/events.

### Steal from mockups (concrete widgets)

- Overview: KPI strip, dual profit+activity chart, subkind donut + priced gauge, top searchers, live ops + unpriced badge (`overview-data-concept` / night-ops pulse).
- Analytics: strategy×time heatmap, exact/inferred bars, net-by-strategy, AMM treemap from `pools[].amm_mix`, log profit-vs-volume scatter with `VOLUME_PARTIAL` tooltip (`analytics-data-concept`).
- Operation: KPI four-up, **profit waterfall** (gross → −gas → −flash if present → net), **two-hop V3 route diagram** from `route[]`, facts table, evidence tags (`long_tail`, `forwarded_profit`, `VOLUME_PARTIAL`), collapsed raw JSON (`operation-data-concept` + forensic waterfall/route).
- Data Health: coverage bars, gap ranges, pricing quality flags, schema caps, reject ladder, storage counts — **dedicated view**, replace the long Details drawer as the primary home for this data.

## Phase 0 — Scaffold (required before Overview)

**Files:** [`core/data/dashboard_template.html`](core/data/dashboard_template.html), [`core/src/explorer/dashboard/render.rs`](core/src/explorer/dashboard/render.rs), add `core/data/echarts.min.js` (or equivalent).

1. Rewrite shell: sidebar nav, `#view-overview|analytics|operations|health`, shared header (chain label from `db.chain_label`, window label, search, synced/coverage chip from `coverage` / `caps.status_label`).
2. Keep `script#snapshot` + `/*__SNAPSHOT__*/` contract.
3. Vendor ECharts offline; update `to_html` if a second placeholder is needed.
4. Shared JS utilities: `usd`, `usdAxisValue` (skip null), `sliceKey(kind,subkind)`, `confidenceBadge`, truncation banner from `panels.truncation`.
5. Replace [`render.rs`](core/src/explorer/dashboard/render.rs) test `template_is_single_composition` — it currently **forbids** nav and requires Fraunces/`#F7F4EF`. New assertions: placeholder present, view ids present, echarts wired, no `</script><script>` breakout, offline (no `fonts.googleapis` / CDN echarts).

## Phase 1 — Overview first

**Data:** existing `kpi`, `series`, `kind_matrix`, `actors`, `events` only.

Widgets:
1. Six KPI cards (Net / Gross / Ops / Searchers / Gas / Pricing coverage). Net/Gross/Gas use ≈ and priced denominators from `*_priced` where useful.
2. Profit & activity: ECharts bar (ops or stacked by slice from matrix+series as available) + cumulative/net line from `series.net_usd`. Unpriced count callout.
3. Operation mix donut from **rolled `kind_matrix` by kind+subkind** (not kind alone).
4. Priced gauge: `ops_priced / ops`.
5. Top searchers from `actors` (ops + net; null-safe).
6. Live operations table from `events` with kind/subkind pills; clicking opens Operations drill-down. Badge: unpriced JIT (or total unpriced) from quality filters.

**Out of scope here:** volume KPI, fake confidence %, branded DEX icons.

## Phase 2 — Analytics (+ reduce aggregates)

**Before** strategy×time heatmap / actor×time charts, extend [`reduce.rs`](core/src/explorer/dashboard/reduce.rs):

Add something like:
- `strategy_series: Vec<{ bucket_start, slice_key, ops, net_usd, priced_ops }>`
- optionally `actor_series` later if needed

Populate in the same `reduce()` pass over full `ops` (pre-truncation). Emit on `ReducedPanels`. Keep CSV/JSON backward compatible (additive fields).

Then Analytics view:
1. Heatmap from `strategy_series` (counts; USD optional secondary).
2. Mix donut + Exact/Inferred bars from `kind_matrix` / `quality`.
3. Net by strategy (priced net only; note unpriced).
4. AMM treemap from `pools[].amm_mix` (and/or token/pool ops).
5. Scatter: points from **full reduce output** — prefer a dedicated `scatter_points` capped list built in Rust from all ops (id, slice, net?, volume?, tags flags), not from truncated browser `events`. Log scales; tooltip shows `VOLUME_PARTIAL` when present in evidence tags/reasons; caption for unpriced excluded from USD axes.

## Phase 3 — Operation drill-down

**Data:** `panels.evidence` keyed by `tx_hash` / id; enrich display via `build_evidence` fields already present.

Layout (match operation-data-concept):
1. Header: kind title, exact/inferred + priced/unpriced badges, copy + explorer link (Snowtrace only if chain_id Avalanche/`43114`).
2. Four metrics: net / gross / gas / volume — each Option-aware.
3. **Waterfall** ECharts: Gross → Gas (−) → Flash fee (− if `Some`) → Net. If flash null/unpriced, omit step and note `flash_fee_unpriced` when flag set.
4. Facts: block, detector, confidence, `pnl_basis`, pricing approx flags, route summary (`N hops · N pools · N AMM families`).
5. **Route diagram:** SVG/HTML from `route[]` — nodes labeled with `amm` + short pool; edge labels humanized amounts when decimals known (`humanize_amount`), else raw/omit. Badge from proximity/token_source when present (e.g. non-registry → “proximity-derived route”). **No Trader Joe/Pangolin labels unless labels map exists.**
6. Evidence: tag pills (`long_tail`, `forwarded_profit`), warning style for `VOLUME_PARTIAL` / reasons, victims list, **`<details>` collapsed raw `details` JSON**.

Wire Operations list view → select op → this panel. Deep-link via `location.hash` e.g. `#/op/<tx_hash>` so refresh works offline.

## Phase 4 — Data Health view

Promote drawer contents into a real view using existing snapshot fields:
- Coverage: head / indexed_to / lag, blocks+ops in window, pricing coverage bar
- Gaps: `gaps.ranges` timeline/list (respect `gaps.enabled` + note)
- Pricing quality: ops_exact/inferred/approx/native_unpriced/multi_asset/clamped/flash_fee_unpriced/excluded
- Schema capabilities: `caps.*` chips + `db.user_version` vs `schema_head`
- Reject ladder: `rejects.ladder` + empty-recording honesty note
- Storage: `storage[]` bar/table
- JIT open / oracle / prices samples as secondary cards

Remove reliance on the old “Show details” drawer as the only access path (keep a small advanced JSON dump if useful).

## Explicit non-goals / later

- Backend server, React app, or CDN chart/fonts
- Inventing registry DEX branding or token icons without data
- Strategy/actor time aggregates **before** Phase 2 reduce work
- Changing detector/PnL semantics in `store/pnl.rs` (dashboard consumes; `VOLUME_PARTIAL` already written there)
- Matching vibe screenshot headline numbers when the open DB differs (current cache ≈ 142 ops / ~$110 net / 15 unpriced / $120B max volume)

## Verification

1. `cargo test -p mev_scout_core explorer::dashboard` (update render/reducer tests).
2. Regenerate: `explorer dashboard --db cache/explorer-avalanche.sqlite --out ... --format html` (and json).
3. Manual HTML checks: Overview KPIs; Analytics scatter tooltip on volume outlier; open top `arb_atomic` → waterfall ≈ gross/gas/net; 12 (or current) unpriced JIT visible in counts not USD; Data Health shows caps/rejects; DevTools network = no external requests.
4. Confirm artifact still escapes `</script>` via `escape_json_for_script`.

## File touch map

| File | Change |
|------|--------|
| [`core/data/dashboard_template.html`](core/data/dashboard_template.html) | Full UI rewrite (phased) |
| `core/data/echarts.min.js` | New vendored asset |
| [`core/src/explorer/dashboard/render.rs`](core/src/explorer/dashboard/render.rs) | Embed echarts; fix tests |
| [`core/src/explorer/dashboard/reduce.rs`](core/src/explorer/dashboard/reduce.rs) | Phase 2: `strategy_series` (+ optional scatter extract) |
| [`core/src/explorer/dashboard/evidence.rs`](core/src/explorer/dashboard/evidence.rs) | Only if drill needs extra typed fields from details |
| [`core/src/explorer/dashboard/mod.rs`](core/src/explorer/dashboard/mod.rs) | Usually untouched; keep semantics |
| [`cli/.../dashboard.rs`](cli/src/commands/explorer/dashboard.rs) | Untouched unless documenting flags |